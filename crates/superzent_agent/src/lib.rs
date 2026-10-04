mod runtime;
mod screen_detection;

pub use runtime::*;
pub use screen_detection::{ScreenAgent, ScreenAgentState, ScreenDetection, identify_screen_agent};
