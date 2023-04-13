#![allow(dead_code)]

use embassy_stm32::gpio::{Input, Output, Pin};
use heapless::Vec;

/// Mark diode direction on the key matrix
pub enum DiodeDirection {
    /// Anode connected to row pins and cathode connected to column pins.
    /// Current flow from row pins to column pins.
    RowColumn = 0,
    /// Cathode connected to row pins and anode connected to column pins.
    /// Current flow from column pins to row pins.
    ColumnRow = 1,
}

// KeyEvent, Up or Down, with index(u8)
pub enum KeyEvent {
    /// Key down event with key index
    Down(u8),
    /// Key up event with key index
    Up(u8),
}

pub struct KeyMatrix<
    'd,
    P: Pin,
    const SIZE_IN: usize,
    const SIZE_OUT: usize,
    const EVENT_COUNT: usize,
> {
    inputs: Vec<Input<'d, P>, SIZE_IN>,
    outputs: Vec<Output<'d, P>, SIZE_OUT>,
    last_results: Vec<Vec<bool, SIZE_OUT>, SIZE_IN>,
    events: Vec<KeyEvent, EVENT_COUNT>,
    diode_direction: DiodeDirection,
}

impl<'d, P: Pin, const SIZE_IN: usize, const SIZE_OUT: usize, const EVENT_COUNT: usize>
    KeyMatrix<'d, P, SIZE_IN, SIZE_OUT, EVENT_COUNT>
{
}
