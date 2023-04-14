#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use defmt::*;

use embassy_executor::Spawner;
use embassy_futures::join::join4;
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Input, Level, Output, OutputOpenDrain, Pull, Speed};
use embassy_stm32::time::Hertz;
use embassy_stm32::usb::Driver;
use embassy_stm32::{interrupt, Config};
use embassy_time::{Duration, Timer};
use embassy_usb::control::OutResponse;
use embassy_usb::Builder;
use {defmt_rtt as _, panic_probe as _};

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;

mod xinput;
use crate::xinput::{
    ReportId, RequestHandler, XinputControlReport, XinputReaderWriter, XinputState,
};

use core::convert::Infallible;
use embassy_stm32::peripherals::{PA1, PA2, PA3, PA4, PA5, PA6, PA7};
use keypad::{embedded_hal::digital::v2::InputPin, keypad_new, keypad_struct};

const VENDOR_STRING: &'static str = "TEST";
const PRODUCT_STRING: &'static str = "TEST CON";
const SERIAL_NUMBER: &'static str = "157F8F9";

keypad_struct! {
    struct MyKeypad<Error = Infallible> {
        rows: (
            Input<'static, PA1>,
            Input<'static, PA2>,
            Input<'static, PA3>,
            Input<'static, PA4>,
        ),
        columns: (
            OutputOpenDrain<'static, PA5>,
            OutputOpenDrain<'static, PA6>,
            OutputOpenDrain<'static, PA7>,
        ),
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut config = Config::default();
    config.rcc.hse = Some(Hertz(8_000_000));
    config.rcc.sys_ck = Some(Hertz(48_000_000));
    config.rcc.pclk1 = Some(Hertz(24_000_000));
    let mut p = embassy_stm32::init(config);

    {
        // BluePill board has a pull-up resistor on the D+ line.
        // Pull the D+ pin down to send a RESET condition to the USB bus.
        // This forced reset is needed only for development, without it host
        // will not reset your device when you upload new firmware.
        let _dp = Output::new(&mut p.PA12, Level::Low, Speed::Low);
        Timer::after(Duration::from_millis(10)).await;
    }

    info!("STM32 Xinput example");

    // Create the driver, from the HAL.
    let irq = interrupt::take!(USB_LP_CAN1_RX0);
    let driver = Driver::new(p.USB, irq, p.PA12, p.PA11);

    // Create embassy-usb Config
    let mut config = embassy_usb::Config::new(0x045e, 0x028e);
    config.max_power = 500;
    config.max_packet_size_0 = 8;
    config.device_class = 0xff;
    config.device_sub_class = 0xff;
    config.device_protocol = 0xff;
    config.device_release = 0x0114; // BCDDevice 1.14
    config.supports_remote_wakeup = true;
    config.manufacturer = Some(VENDOR_STRING);
    config.product = Some(PRODUCT_STRING);
    config.serial_number = Some(SERIAL_NUMBER);
    config.self_powered = true;

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut device_descriptor = [0; 256];
    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut control_buf = [0; 64];
    let request_handler = MyRequestHandler {};

    let mut state = XinputState::new();

    // Note: We actually don't need BOS descriptor. It's easy to change. But I'll keep it.
    let mut builder = Builder::new(
        driver,
        config,
        &mut device_descriptor,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut control_buf,
    );

    // Create classes on the builder.
    let config = crate::xinput::Config {
        vendor_string: Some(VENDOR_STRING),
        product_string: Some(PRODUCT_STRING),
        serial_number_string: Some(SERIAL_NUMBER),
        request_handler: Some(&request_handler),
        ..Default::default()
    };
    let xinput = XinputReaderWriter::<_>::new(&mut builder, &mut state, config);

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device. Well, here's only the future to run.
    let usb_fut = usb.run();

    // previously I use a single button to test
    // this might be developed to a button for special functions
    // I need abstraction.
    let mut _button = ExtiInput::new(Input::new(p.PA0, Pull::Down), p.EXTI0);

    let (reader, mut writer) = xinput.split();

    // prepare the keypad
    let keypad = keypad_new!(MyKeypad {
        rows: (
            Input::new(p.PA1, Pull::Up),
            Input::new(p.PA2, Pull::Up),
            Input::new(p.PA3, Pull::Up),
            Input::new(p.PA4, Pull::Up),
        ),
        columns: (
            OutputOpenDrain::new(p.PA5, Level::High, Speed::VeryHigh, Pull::Down),
            OutputOpenDrain::new(p.PA6, Level::High, Speed::VeryHigh, Pull::Down),
            OutputOpenDrain::new(p.PA7, Level::High, Speed::VeryHigh, Pull::Down),
        ),
    });

    let keys = keypad.decompose();

    // communication between tasks
    let channel = Channel::<NoopRawMutex, (bool, (usize, usize)), 24>::new();
    let sender = channel.sender();
    let receiver = channel.receiver();

    // scan keys and generate key events
    let keypad_fut = async {
        let mut button_states = [false; 12];
        loop {
            for (row_index, row) in keys.iter().enumerate() {
                for (col_index, key) in row.iter().enumerate() {
                    let current_state = if key.is_low().unwrap() { true } else { false };
                    match (current_state, button_states[col_index * 4 + row_index]) {
                        (true, false) => {
                            info!("Key {} pressed", (row_index, col_index));
                            button_states[col_index * 4 + row_index] = current_state;
                            sender.send((current_state, (row_index, col_index))).await;
                        }
                        (false, true) => {
                            info!("Key {} released", (row_index, col_index));
                            button_states[col_index * 4 + row_index] = current_state;
                            sender.send((current_state, (row_index, col_index))).await;
                        }
                        _ => {}
                    }
                }
            }
            Timer::after(Duration::from_hz(120)).await; // also debounce
        }
    };

    // Process key events
    let in_fut = async {
        let mut controller = XinputControlReport::default();

        loop {
            let (status, button) = receiver.recv().await;

            let _ = match button {
                (0, 0) => controller.dpad_right = status,
                (1, 0) => controller.dpad_up = status,
                (2, 0) => controller.dpad_left = status,
                (3, 0) => controller.dpad_down = status,
                (0, 1) => controller.button_b = status,
                (1, 1) => controller.button_y = status,
                (2, 1) => controller.button_x = status,
                (3, 1) => controller.button_a = status,
                (0, 2) => controller.button_view = status,
                (1, 2) => controller.button_menu = status,
                (2, 2) => controller.shoulder_left = status,
                (3, 2) => controller.shoulder_right = status,
                _ => {}
            };

            match writer.write_control(&controller).await {
                Ok(()) => {}
                Err(e) => warn!("Failed to send report: {:?}", e),
            };
        }
    };

    // read report from USB host
    // basically rumble and led status
    let out_fut = async {
        reader.run(false, &request_handler).await;
    };

    // Run everything concurrently.
    // If we had made everything `'static` above instead, we could do this using separate tasks instead.
    join4(usb_fut, in_fut, out_fut, keypad_fut).await;
}

struct MyRequestHandler {}

impl RequestHandler for MyRequestHandler {
    fn get_report(&self, id: ReportId, _buf: &mut [u8]) -> Option<usize> {
        info!("Get report for {:?}", id);
        None
    }

    fn set_report(&self, id: ReportId, data: &[u8]) -> OutResponse {
        info!("Set report for {:?}: {=[u8]}", id, data);
        OutResponse::Accepted
    }
}
