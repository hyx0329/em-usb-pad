#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use defmt::*;

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::time::Hertz;
use embassy_stm32::usb::Driver;
use embassy_stm32::{interrupt, Config};
use embassy_time::{Duration, Timer};
use embassy_usb::control::OutResponse;
use embassy_usb::{Builder};
use {defmt_rtt as _, panic_probe as _};

mod xinput;
use crate::xinput::{
    ReportId, RequestHandler, XinputControlReport, XinputReaderWriter, XinputState,
};

mod keymatrix;

const VENDOR_STRING: &'static str = "TEST";
const PRODUCT_STRING: &'static str = "TEST CON";
const SERIAL_NUMBER: &'static str = "157F8F9";

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

    // Run the USB device.
    let usb_fut = usb.run();

    let mut button = ExtiInput::new(Input::new(p.PA0, Pull::Down), p.EXTI0);

    let (reader, mut writer) = xinput.split();

    // Do stuff with the class!
    let in_fut = async {
        loop {
            button.wait_for_high().await;
            info!("PRESSED");

            let report = XinputControlReport {
                button_a: true,
                ..Default::default()
            };

            match writer.write_control(&report).await {
                Ok(()) => {}
                Err(e) => warn!("Failed to send report: {:?}", e),
            };

            button.wait_for_low().await;
            info!("RELEASED");

            let report = XinputControlReport {
                button_a: false,
                ..report
            };

            match writer.write_control(&report).await {
                Ok(()) => {}
                Err(e) => warn!("Failed to send report: {:?}", e),
            };
        }
    };

    let out_fut = async {
        reader.run(false, &request_handler).await;
    };

    // Run everything concurrently.
    // If we had made everything `'static` above instead, we could do this using separate tasks instead.
    join(usb_fut, join(in_fut, out_fut)).await;
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
