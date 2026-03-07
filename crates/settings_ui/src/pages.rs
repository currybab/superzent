mod audio_input_output_setup;
#[cfg(any(test, feature = "test-support"))]
mod audio_test_window;
#[cfg(any(test, feature = "test-support"))]
mod edit_prediction_provider_setup;
#[cfg(any(test, feature = "test-support"))]
mod tool_permissions_setup;

pub(crate) use audio_input_output_setup::{
    render_input_audio_device_dropdown, render_output_audio_device_dropdown,
};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use audio_test_window::open_audio_test_window;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use edit_prediction_provider_setup::render_edit_prediction_setup_page;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use tool_permissions_setup::render_tool_permissions_setup_page;

#[cfg(any(test, feature = "test-support"))]
pub use tool_permissions_setup::{
    render_copy_path_tool_config, render_create_directory_tool_config,
    render_delete_path_tool_config, render_edit_file_tool_config, render_fetch_tool_config,
    render_move_path_tool_config, render_restore_file_from_disk_tool_config,
    render_save_file_tool_config, render_terminal_tool_config, render_web_search_tool_config,
};
