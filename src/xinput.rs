#![allow(unused_macros)]
#![allow(dead_code)]

use core::mem::MaybeUninit;
use packed_struct::prelude::*;

use embassy_usb::control::OutResponse;
use embassy_usb::driver::{Driver, Endpoint, EndpointError, EndpointIn, EndpointOut};
use embassy_usb::{Builder,Handler};

use defmt::{trace, warn};

// For Xinput controllers, there are 4 USB interfaces:
// - Control
// - Audio (and possibly expansion port)
// - Unknown (whatever)
// - Security
// each interface may have several endpoints to use
// btw, embassy-usb limit the usb interface number to 4, which is just enough
// The Xinput protocol is NOT a variant of USB HID, it's a fully customized one.

// just copied from a controller with Xinput support
const USB_XINPUT_VID: u16 = 0x045e;
const USB_XINPUT_PID: u16 = 0x028e;
const USB_CLASS_VENDOR: u8 = 0xff;
const USB_SUBCLASS_VENDOR: u8 = 0xff;
const USB_PROTOCOL_VENDOR: u8 = 0xff;
const USB_DEVICE_RELEASE: u16 = 0x0114;

// the following descriptors copied & adapted from the link below & my own controller
// github.com/dmadison/ArduinoXinput_AVR

// NOTE: the following string may vary on different 3rd-party controllers
// Since we do not communicate with XBox consoles, the SN doesn't make sense.
const XINPUT_DESC_STRING_VENDOR: &str = "Embassy";
const XINPUT_DESC_STRING_PRODUCT: &str = "Pad Oxide";
const XINPUT_DESC_STRING_SN: &str = "Controller";
const XINPUT_DESC_STRING_SECURITY: &str =
    "Pad Oxide does not support Xbox Security Method!";

const XINPUT_DESC_DESCTYPE_STANDARD: u8 = 0x21; // a common descriptor type for all xinput interfaces
const XINPUT_DESC_DESCTYPE_SECURITY: u8 = 0x41; // a special one for the security descriptor
const XINPUT_IFACE_SUBCLASS_STANDARD: u8 = 0x5D;
const XINPUT_IFACE_SUBCLASS_SECURITY: u8 = 0xFD;

const XINPUT_IFACE_PROTO_IF0: u8 = 0x01;
const XINPUT_IFACE_PROTO_IF1: u8 = 0x03;
const XINPUT_IFACE_PROTO_IF2: u8 = 0x02;
const XINPUT_IFACE_PROTO_IF3: u8 = 0x13;

const XINPUT_EP_MAX_PACKET_SIZE: u16 = 0x20;
const XINPUT_RW_BUFFER_SIZE: usize = XINPUT_EP_MAX_PACKET_SIZE as usize;

const XINPUT_DESC_IF0: &[u8] = &[
    // for control interface
    0x00, 0x01, 0x01, 0x25, // ???
    0x81, // bEndpointAddress (IN, 1)
    0x14, // bMaxDataSize
    0x00, 0x00, 0x00, 0x00, 0x13, // ???
    0x01, // bEndpointAddress (OUT, 1)
    0x08, // bMaxDataSize
    0x00, 0x00, // ???
];
const XINPUT_DESC_IF1: &[u8] = &[
    // for audio and expansion(possibly)
    0x00, 0x01, 0x01, 0x01, // ???
    0x82, // bEndpointAddress (IN, 2)
    0x40, // bMaxDataSize
    0x01, // ???
    0x02, // bEndpointAddress (OUT, 2)
    0x20, // bMaxDataSize
    0x16, // ???
    0x83, // bEndpointAddress (IN, 3)
    0x00, // bMaxDataSize
    0x00, 0x00, 0x00, 0x00, 0x00, 0x16, // ???
    0x03, // bEndpointAddress (OUT, 3)
    0x00, // bMaxDataSize
    0x00, 0x00, 0x00, 0x00, 0x00, // ???
];
const XINPUT_DESC_IF2: &[u8] = &[
    // for an unknown interface
    0x00, 0x01, 0x01, 0x22, // ???
    0x84, // bEndpointAddress (IN, 4)
    0x07, // bMaxDataSize
    0x00, // ???
];
const XINPUT_DESC_IF3: &[u8] = &[
    // for security interface
    0x00, 0x01, 0x01, 0x03, // ???
];

/// Interface to handle
pub trait RequestHandler {
    /// Reads the value of report `id` into `buf` returning the size.
    ///
    /// Returns `None` if `id` is invalid or no data is available.
    fn get_report(&self, id: ReportId, buf: &mut [u8]) -> Option<usize> {
        let _ = (id, buf);
        None
    }

    /// Sets the value of report `id` to `data`.
    /// That means the msg from host is received and processed/saved.
    fn set_report(&self, id: ReportId, data: &[u8]) -> OutResponse {
        let _ = (id, data);
        OutResponse::Rejected
    }
}

/// The ability to convert struct to a buffer to send
pub trait AsXinputReport {
    /// Write serialized report to the given buffer start from given offset
    /// The report will be directly sent by `[XinputWriter::write]`
    /// For interface0, the report size should not exceed 32
    ///
    /// Returns the full report length.
    fn to_report(&self, offset: usize, buf: &mut [u8]) -> usize;
}

/// Store the input states of the controller
#[derive(PackedStruct, Default, Debug, PartialEq)]
#[packed_struct(endian = "lsb", bit_numbering = "msb0")]
pub struct XinputControlReport {
    // byte zero
    #[packed_field(bits = "0")]
    pub thumb_click_right: bool,
    #[packed_field(bits = "1")]
    pub thumb_click_left: bool,
    #[packed_field(bits = "2")]
    pub button_view: bool,
    #[packed_field(bits = "3")]
    pub button_menu: bool,
    #[packed_field(bits = "4")]
    pub dpad_right: bool,
    #[packed_field(bits = "5")]
    pub dpad_left: bool,
    #[packed_field(bits = "6")]
    pub dpad_down: bool,
    #[packed_field(bits = "7")]
    pub dpad_up: bool,
    // byte one
    #[packed_field(bits = "8")]
    pub button_y: bool,
    #[packed_field(bits = "9")]
    pub button_x: bool,
    #[packed_field(bits = "10")]
    pub button_b: bool,
    #[packed_field(bits = "11")]
    pub button_a: bool,
    // #[packed_field(bits = "12")]
    // pub reserved: bool,
    #[packed_field(bits = "13")]
    pub xbox_button: bool,
    #[packed_field(bits = "14")]
    pub shoulder_right: bool,
    #[packed_field(bits = "15")]
    pub shoulder_left: bool,
    // others
    #[packed_field(bytes = "2")]
    pub trigger_left: u8,
    #[packed_field(bytes = "3")]
    pub trigger_right: u8,
    #[packed_field(bytes = "4..=5")]
    pub js_left_x: i16,
    #[packed_field(bytes = "6..=7")]
    pub js_left_y: i16,
    #[packed_field(bytes = "8..=9")]
    pub js_right_x: i16,
    #[packed_field(bytes = "10..=11")]
    pub js_right_y: i16,
}

impl XinputControlReport {
    fn new() -> Self {
        XinputControlReport::default()
    }
}

impl AsXinputReport for XinputControlReport {
    fn to_report(&self, offset: usize, buf: &mut [u8]) -> usize {
        let packed = self.pack().unwrap();
        let length: usize = 20;
        assert!(20 <= buf.len() - offset); // total length < available buf
        buf[0] = 0; // packet type id
        buf[1] = length as u8; // packet length
        for (i, v) in packed.iter().enumerate() {
            buf[i + 2 + offset] = *v;
        }
        for i in (packed.len() + 2)..length {
            buf[i] = 0;
        }
        length
    }
}

#[derive(Default)]
pub struct XinputRumbleState {
    left: u8,
    right: u8,
}

#[repr(u8)]
#[derive(PrimitiveEnum_u8, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XinputLedPattern {
    Off = 0x00,
    Blink = 0x01,
    Flash1 = 0x02,
    Flash2 = 0x03,
    Flash3 = 0x04,
    Flash4 = 0x05,
    On1 = 0x06,
    On2 = 0x07,
    On3 = 0x08,
    On4 = 0x09,
    Rotate = 0x0A,
    BlinkOnce = 0x0B,
    BlinkSlow = 0x0C,
    Alternate = 0x0D,
}

pub enum XinputHostStatus {
    /// Rumble status
    Rumble(XinputRumbleState),
    /// LED status
    Led(XinputLedPattern),
    /// Unknown status packet, not parsed
    Unknown,
}

impl From<&[u8]> for XinputHostStatus {
    /// Build XinputHostStatus from raw host report
    fn from(value: &[u8]) -> Self {
        match (value[0], value.len()) {
            (0x00, 0x08) => XinputHostStatus::Rumble(XinputRumbleState {
                left: value[3],
                right: value[4],
            }),
            (0x01, 0x03) => match XinputLedPattern::from_primitive(value[2]) {
                Some(led_pattern) => XinputHostStatus::Led(led_pattern),
                None => XinputHostStatus::Unknown,
            },
            _ => XinputHostStatus::Unknown,
        }
    }
}

pub struct Config<'d> {
    // STRING descriptors
    pub vendor_string: Option<&'d str>,
    pub product_string: Option<&'d str>,
    pub serial_number_string: Option<&'d str>,
    pub security_string: Option<&'d str>,

    // Handlers for different interfaces.
    /// Control and LED handlers
    pub request_handler: Option<&'d dyn RequestHandler>, // mimic hid
    /// Audio and accessary handlers
    pub audio_handler: Option<&'d dyn RequestHandler>, // subject to change
    /// A handler for an unknown interface
    pub unknown_handler: Option<&'d dyn RequestHandler>, // subject to change
    /// A handler for security interface
    pub security_handler: Option<&'d dyn RequestHandler>, // subject to change
}

impl<'d> Default for Config<'d> {
    fn default() -> Self {
        Config {
            vendor_string: Some(XINPUT_DESC_STRING_VENDOR),
            product_string: Some(XINPUT_DESC_STRING_PRODUCT),
            serial_number_string: Some(XINPUT_DESC_STRING_SN),
            security_string: Some(XINPUT_DESC_STRING_SECURITY),

            request_handler: None,
            audio_handler: None,
            unknown_handler: None,
            security_handler: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum ReportId {
    In(u8),
    Out(u8),
    Feature(u8),
}

impl ReportId {
    fn try_from(value: u16) -> Result<Self, ()> {
        match value >> 8 {
            1 => Ok(ReportId::In(value as u8)),
            2 => Ok(ReportId::Out(value as u8)),
            3 => Ok(ReportId::Feature(value as u8)),
            _ => Err(()),
        }
    }
}

struct Control<'d> {
    vendor_string: Option<&'d str>,
    product_string: Option<&'d str>,
    serial_number_string: Option<&'d str>,
    security_string: Option<&'d str>,
    request_handler: Option<&'d dyn RequestHandler>,
}

impl<'d> Control<'d> {
    fn new(
        vendor_string: Option<&'d str>,
        product_string: Option<&'d str>,
        serial_number_string: Option<&'d str>,
        security_string: Option<&'d str>,
        request_handler: Option<&'d dyn RequestHandler>,
    ) -> Self {
        Control {
            vendor_string,
            product_string,
            serial_number_string,
            security_string,
            request_handler,
        }
    }
}

impl<'d> Handler for Control<'d> {
    fn get_string(&mut self, index: embassy_usb::types::StringIndex, lang_id: u16) -> Option<&str> {
        trace!("Xinput get_descriptor string");
        let _ = lang_id;
        match u8::from(index) {
            0x01 => self.vendor_string,
            0x02 => self.product_string,
            0x03 => self.serial_number_string,
            0x04 => self.security_string,
            _ => None,
        }
    }
}

/// A shared state of interface status
pub struct XinputState<'d> {
    control_control: MaybeUninit<Control<'d>>,
}

impl<'d> XinputState<'d> {
    pub fn new() -> Self {
        XinputState {
            control_control: MaybeUninit::uninit(),
        }
    }
}

pub struct XinputReaderWriter<'d, D: Driver<'d>> {
    reader: XinputReader<'d, D>,
    writer: XinputWriter<'d, D>,
}

pub struct XinputWriter<'d, D: Driver<'d>> {
    ep_in: D::EndpointIn,
}

pub struct XinputReader<'d, D: Driver<'d>> {
    ep_out: D::EndpointOut,
}

#[derive(Debug, Clone, PartialEq, Eq, defmt::Format)]
pub enum ReadError {
    BufferOverflow,
    Disabled,
}

impl From<EndpointError> for ReadError {
    fn from(val: EndpointError) -> Self {
        use EndpointError::*;
        match val {
            BufferOverflow => ReadError::BufferOverflow,
            Disabled => ReadError::Disabled,
        }
    }
}

impl<'d, D: Driver<'d>> XinputWriter<'d, D> {
    /// Waits for the interrupt in endpoint to be enabled.
    pub async fn ready(&mut self) -> () {
        self.ep_in.wait_enabled().await
    }

    /// Write controller status by serializing the report structure
    pub async fn write_control(
        &mut self,
        report: &dyn AsXinputReport,
    ) -> Result<(), EndpointError> {
        let mut buf: [u8; XINPUT_RW_BUFFER_SIZE] = [0; XINPUT_RW_BUFFER_SIZE];
        let length = report.to_report(0, &mut buf);
        trace!("Write controller data: {}", &buf[0..length]);
        self.write(&buf[0..length]).await
    }

    /// Writes `report` to its interrupt endpoint.
    /// no packet length check
    pub async fn write(&mut self, report: &[u8]) -> Result<(), EndpointError> {
        assert!(report.len() <= XINPUT_RW_BUFFER_SIZE);
        self.ep_in.write(report).await?;
        Ok(())
    }
}

impl<'d, D: Driver<'d>> XinputReader<'d, D> {
    /// Waits for the interrupt out endpoint to be enabled.
    pub async fn ready(&mut self) -> () {
        self.ep_out.wait_enabled().await
    }

    /// Delivers output reports from the Interrupt Out pipe to `handler`.
    pub async fn run<T: RequestHandler>(mut self, use_report_ids: bool, handler: &T) -> ! {
        let mut buf = [0; XINPUT_RW_BUFFER_SIZE];
        loop {
            match self.read(&mut buf).await {
                Ok(len) => {
                    let id = if use_report_ids { buf[0] } else { 0 };
                    handler.set_report(ReportId::Out(id), &buf[..len]);
                }
                Err(ReadError::BufferOverflow) => warn!(
                    "Host sent output report larger than the configured maximum output report length ({})", XINPUT_EP_MAX_PACKET_SIZE),
                Err(ReadError::Disabled) => self.ep_out.wait_enabled().await,
            }
        }
    }

    /// Reads an output report from the Interrupt Out pipe.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ReadError> {
        // Read packets from the endpoint, ignoring packets bigger than XINPUT_EP_MAX_PACKET_SIZE
        // The max_packet_size is bigger than XINPUT_EP_MAX_PACKET_SIZE so it should work under most circumstances
        let max_packet_size = usize::from(self.ep_out.info().max_packet_size);
        assert!(buf.len() >= max_packet_size);

        loop {
            match self.ep_out.read(buf).await {
                Ok(size) => {
                    assert!(size <= max_packet_size);
                    if 0 < size {
                        // not empty, what we need
                        return Ok(size);
                    }
                }
                Err(err) => {
                    return Err(err.into());
                }
            }
        }
    }
}

/// Create all 4 interfaces for xinput
fn build<'d, D: Driver<'d>>(
    builder: &mut Builder<'d, D>,
    state: &'d mut XinputState<'d>,
    config: Config<'d>,
) -> (
    Option<D::EndpointOut>,
    Option<D::EndpointIn>,
    Option<D::EndpointOut>,
    Option<D::EndpointIn>,
    Option<D::EndpointOut>,
    Option<D::EndpointIn>,
    Option<D::EndpointIn>,
) {
    // add the handler in advance so no mut ref more than once error
    let control = state.control_control.write(Control::new(
        config.vendor_string,
        config.product_string,
        config.serial_number_string,
        config.security_string,
        config.request_handler,
    ));
    builder.handler(control);

    // add a new configuration
    let mut func = builder.function(USB_CLASS_VENDOR, USB_SUBCLASS_VENDOR, USB_PROTOCOL_VENDOR);

    // initialize control interface, which is the most important one
    // steps:
    // - optionally prepare the handlers
    // - create interface
    // - setup alt descriptor
    // - setup endpoint
    // interface/endpoint descriptor order matters!

    let mut control_interface = func.interface();
    let mut alt_control = control_interface.alt_setting(
        USB_CLASS_VENDOR,
        XINPUT_IFACE_SUBCLASS_STANDARD,
        XINPUT_IFACE_PROTO_IF0,
        None,
    );
    alt_control.descriptor(XINPUT_DESC_DESCTYPE_STANDARD, XINPUT_DESC_IF0);
    let ep_in_if0 = alt_control.endpoint_interrupt_in(XINPUT_EP_MAX_PACKET_SIZE, 0x04);
    let ep_out_if0 = alt_control.endpoint_interrupt_out(XINPUT_EP_MAX_PACKET_SIZE, 0x08);
    // allocate the 4th string descriptor
    let str_index = control_interface.string();
    assert!(
        4 == u8::from(str_index),
        "The extra str_index should be 4 but it's {} !",
        u8::from(str_index)
    );

    // the audio interface
    let mut audio_interface = func.interface();
    let mut alt_audio = audio_interface.alt_setting(
        USB_CLASS_VENDOR,
        XINPUT_IFACE_SUBCLASS_STANDARD,
        XINPUT_IFACE_PROTO_IF1,
        None,
    );
    alt_audio.descriptor(XINPUT_DESC_DESCTYPE_STANDARD, XINPUT_DESC_IF1);
    let ep_in_if1_1 = alt_audio.endpoint_interrupt_in(XINPUT_EP_MAX_PACKET_SIZE, 0x02);
    let ep_out_if1_1 = alt_audio.endpoint_interrupt_out(XINPUT_EP_MAX_PACKET_SIZE, 0x04);
    let ep_in_if1_2 = alt_audio.endpoint_interrupt_in(XINPUT_EP_MAX_PACKET_SIZE, 0x40);
    let ep_out_if1_2 = alt_audio.endpoint_interrupt_out(XINPUT_EP_MAX_PACKET_SIZE, 0x10);

    // the unknown one
    let mut unknown_interface = func.interface();
    let mut alt_unknown = unknown_interface.alt_setting(
        USB_CLASS_VENDOR,
        XINPUT_IFACE_SUBCLASS_STANDARD,
        XINPUT_IFACE_PROTO_IF2,
        None,
    );
    alt_unknown.descriptor(XINPUT_DESC_DESCTYPE_STANDARD, XINPUT_DESC_IF2);
    let ep_in_if2 = alt_unknown.endpoint_interrupt_in(XINPUT_EP_MAX_PACKET_SIZE, 0x10);

    // the security interface, no endpoint
    let mut security_interface = func.interface();
    let mut alt_security = security_interface.alt_setting(
        USB_CLASS_VENDOR,
        XINPUT_IFACE_SUBCLASS_SECURITY,
        XINPUT_IFACE_PROTO_IF3,
        None,
    );
    alt_security.descriptor(XINPUT_DESC_DESCTYPE_SECURITY, XINPUT_DESC_IF3);

    (
        Some(ep_out_if0),
        Some(ep_in_if0),
        Some(ep_out_if1_1),
        Some(ep_in_if1_1),
        Some(ep_out_if1_2),
        Some(ep_in_if1_2),
        Some(ep_in_if2),
    )
}

impl<'d, D: Driver<'d>> XinputReaderWriter<'d, D> {
    /// Create a new XinputReaderWriter
    pub fn new(
        builder: &mut Builder<'d, D>,
        state: &'d mut XinputState<'d>,
        config: Config<'d>,
    ) -> Self {
        let endpoints = build(builder, state, config);
        let (control_out, control_in, _, _, _, _, _) = endpoints;
        Self {
            reader: XinputReader {
                ep_out: control_out.unwrap(),
            },
            writer: XinputWriter {
                ep_in: control_in.unwrap(),
            },
        }
    }

    /// Splits into seperate readers/writers for input and output reports.
    pub fn split(self) -> (XinputReader<'d, D>, XinputWriter<'d, D>) {
        (self.reader, self.writer)
    }

    /// Waits for both IN and OUT endpoints to be enabled.
    pub async fn ready(&mut self) -> () {
        self.reader.ready().await;
        self.writer.ready().await;
    }

    /// Writes `report` to its interrupt endpoint.
    pub async fn write(&mut self, report: &[u8]) -> Result<(), EndpointError> {
        self.writer.write(report).await
    }

    /// Writes an input report with a structure
    pub async fn write_control(
        &mut self,
        report: &XinputControlReport,
    ) -> Result<(), EndpointError> {
        self.writer.write_control(report).await
    }

    /// Reads an output report from the Interrupt Out pipe.
    ///
    /// See [`XinputReader::read`].
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ReadError> {
        self.reader.read(buf).await
    }
}
