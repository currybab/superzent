mod audio_input_output_setup;
#[allow(dead_code)]
#[cfg(test)]
mod audio_test_window;
#[allow(dead_code)]
#[cfg(feature = "ai")]
mod edit_prediction_provider_setup;
mod superzent_agent_presets;
#[allow(dead_code)]
#[cfg(feature = "ai")]
mod tool_permissions_setup;

pub(crate) use audio_input_output_setup::{
    render_input_audio_device_dropdown, render_output_audio_device_dropdown,
};
pub(crate) use superzent_agent_presets::render_superzent_agent_presets_page;

#[cfg(feature = "ai")]
pub use tool_permissions_setup::{
    render_copy_path_tool_config, render_create_directory_tool_config,
    render_delete_path_tool_config, render_edit_file_tool_config, render_fetch_tool_config,
    render_move_path_tool_config, render_restore_file_from_disk_tool_config,
    render_save_file_tool_config, render_terminal_tool_config, render_web_search_tool_config,
};
