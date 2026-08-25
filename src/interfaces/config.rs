use crate::common::config::{Config, WorkspaceSelector};
use crate::core::config::{AnimationConfig, CoreConfig, Gaps, LayoutConfig};
use crate::core::error::CoreError;
use crate::core::ids::{DisplayId, WorkspaceNumber};
use crate::core::rules::{WindowRule, WorkspaceTarget};

pub fn core_config(config: &Config) -> Result<CoreConfig, CoreError> {
    let window_rules = config
        .virtual_workspaces
        .app_rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            let workspace = match &rule.workspace {
                Some(WorkspaceSelector::Index(number)) => {
                    let number = u8::try_from(*number).map_err(|_| {
                        CoreError::InvalidCommand(format!(
                            "window rule {index} workspace number is out of range: {number}"
                        ))
                    })?;
                    WorkspaceTarget::Number(WorkspaceNumber::try_from(number).map_err(|error| {
                        CoreError::InvalidCommand(format!(
                            "window rule {index} has invalid workspace number: {error}"
                        ))
                    })?)
                }
                Some(WorkspaceSelector::Name(name)) => WorkspaceTarget::Name(name.clone()),
                None => WorkspaceTarget::Current,
            };
            Ok(WindowRule {
                app_id: rule.app_id.clone(),
                app_name: rule.app_name.clone(),
                title_regex: rule.title_regex.clone(),
                title_substring: rule.title_substring.clone(),
                ax_role: rule.ax_role.clone(),
                ax_subrole: rule.ax_subrole.clone(),
                workspace,
                floating: rule.floating,
                manage: rule.manage,
            })
        })
        .collect::<Result<Vec<_>, CoreError>>()?;

    let default_gaps = translate_gaps(&config.settings.layout.gaps);
    let gaps_by_display = config
        .settings
        .layout
        .gaps
        .per_display
        .keys()
        .map(|display| {
            let resolved = config.settings.layout.gaps.effective_for_display(Some(display));
            (DisplayId(display.clone()), translate_gaps(&resolved))
        })
        .collect();
    let translated = CoreConfig {
        focus_follows_mouse: config.settings.focus_follows_mouse,
        mouse_follows_focus: config.settings.mouse_follows_focus,
        mouse_hides_on_focus: config.settings.mouse_hides_on_focus,
        drag_swap_fraction: config.settings.window_snapping.drag_swap_fraction,
        auto_destroy_empty_workspaces: true,
        display_migration_priority: config
            .virtual_workspaces
            .display_migration_priority
            .iter()
            .cloned()
            .map(DisplayId)
            .collect(),
        window_rules,
        layout: LayoutConfig {
            gaps: default_gaps,
            gaps_by_display,
        },
        animation: AnimationConfig {
            enabled: config.settings.animate,
            duration_seconds: config.settings.animation_duration,
            frames_per_second: config.settings.animation_fps,
        },
    };
    crate::core::rules::RuleSet::compile(translated.window_rules.clone())?;
    Ok(translated)
}

fn translate_gaps(gaps: &crate::common::config::GapSettings) -> Gaps {
    Gaps {
        top: gaps.outer.top,
        left: gaps.outer.left,
        bottom: gaps.outer.bottom,
        right: gaps.outer.right,
        horizontal: gaps.inner.horizontal,
        vertical: gaps.inner.vertical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_translates_to_valid_core_config() {
        let config = Config::default();
        let translated = core_config(&config).unwrap();
        assert_eq!(
            translated.focus_follows_mouse,
            config.settings.focus_follows_mouse
        );
        assert_eq!(
            translated.window_rules.len(),
            config.virtual_workspaces.app_rules.len()
        );
        assert!(translated.display_migration_priority.is_empty());
    }

    #[test]
    fn display_gap_overrides_are_resolved_at_the_config_boundary() {
        let mut config = Config::default();
        config.settings.layout.gaps.outer.left = 8.0;
        config.settings.layout.gaps.per_display.insert(
            "external".into(),
            crate::common::config::GapOverride {
                outer: Some(crate::common::config::OuterGaps {
                    left: 24.0,
                    ..Default::default()
                }),
                inner: None,
            },
        );

        let translated = core_config(&config).unwrap();
        assert_eq!(translated.layout.gaps.left, 8.0);
        assert_eq!(
            translated.layout.gaps_for(&DisplayId("external".into())).left,
            24.0
        );
    }
}
