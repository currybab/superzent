use settings::{RegisterSetting, Settings, SettingsContent};

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, RegisterSetting)]
pub struct TitleBarSettings {
    pub show_branch_icon: bool,
    pub show_onboarding_banner: bool,
    pub show_user_picture: bool,
    pub show_branch_name: bool,
    pub show_project_items: bool,
    pub show_sign_in: bool,
    pub show_user_menu: bool,
    pub show_menus: bool,
    pub show_resource_monitor: bool,
}

impl Settings for TitleBarSettings {
    fn from_settings(s: &SettingsContent) -> Self {
        let content = s.title_bar.clone().unwrap();
        TitleBarSettings {
            show_branch_icon: content.show_branch_icon.unwrap(),
            show_onboarding_banner: content.show_onboarding_banner.unwrap(),
            show_user_picture: content.show_user_picture.unwrap(),
            show_branch_name: content.show_branch_name.unwrap(),
            show_project_items: content.show_project_items.unwrap(),
            show_sign_in: content.show_sign_in.unwrap(),
            show_user_menu: content.show_user_menu.unwrap(),
            show_menus: content.show_menus.unwrap(),
            show_resource_monitor: content.show_resource_monitor.unwrap(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_settings_reads_resource_monitor_flag() {
        let mut settings = SettingsContent::default();
        let title_bar = settings.title_bar.get_or_insert_default();
        title_bar.show_branch_icon = Some(false);
        title_bar.show_onboarding_banner = Some(false);
        title_bar.show_user_picture = Some(false);
        title_bar.show_branch_name = Some(true);
        title_bar.show_project_items = Some(true);
        title_bar.show_sign_in = Some(false);
        title_bar.show_user_menu = Some(false);
        title_bar.show_menus = Some(false);
        title_bar.show_resource_monitor = Some(true);

        let title_bar_settings = TitleBarSettings::from_settings(&settings);

        assert!(title_bar_settings.show_resource_monitor);
    }
}
