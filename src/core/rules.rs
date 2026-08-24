use regex::Regex;
use serde::{Deserialize, Serialize};

use super::error::CoreError;
use super::ids::WorkspaceNumber;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTarget {
    Number(WorkspaceNumber),
    Name(String),
    Current,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WindowRule {
    pub app_id: Option<String>,
    pub app_name: Option<String>,
    pub title_regex: Option<String>,
    pub title_substring: Option<String>,
    pub ax_role: Option<String>,
    pub ax_subrole: Option<String>,
    pub workspace: WorkspaceTarget,
    pub floating: bool,
    pub manage: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowIdentity<'a> {
    pub app_id: Option<&'a str>,
    pub app_name: Option<&'a str>,
    pub title: Option<&'a str>,
    pub ax_role: Option<&'a str>,
    pub ax_subrole: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleDecision {
    Managed {
        workspace: WorkspaceTarget,
        floating: bool,
        rule_index: Option<usize>,
    },
    Unmanaged {
        rule_index: usize,
    },
}

#[derive(Clone, Debug, Default)]
pub struct RuleSet {
    rules: Vec<CompiledRule>,
}

#[derive(Clone, Debug)]
struct CompiledRule {
    source: WindowRule,
    title_regex: Option<Regex>,
}

impl RuleSet {
    pub fn compile(rules: Vec<WindowRule>) -> Result<Self, CoreError> {
        let mut compiled = Vec::with_capacity(rules.len());
        for (index, source) in rules.into_iter().enumerate() {
            let title_regex = match source.title_regex.as_deref() {
                Some("") | None => None,
                Some(pattern) => Some(
                    regex::RegexBuilder::new(pattern)
                        .case_insensitive(true)
                        .build()
                        .map_err(|error| {
                            CoreError::InvalidCommand(format!(
                                "window rule {index} has invalid title regex: {error}"
                            ))
                        })?,
                ),
            };
            compiled.push(CompiledRule { source, title_regex });
        }
        Ok(Self { rules: compiled })
    }

    pub fn decide(&self, identity: WindowIdentity<'_>) -> RuleDecision {
        let matches = self
            .rules
            .iter()
            .enumerate()
            .filter_map(|(index, rule)| {
                rule.matches(identity)
                    .then_some((index, rule, rule.specificity()))
            })
            .collect::<Vec<_>>();

        let grouped_app_id = matches
            .iter()
            .filter_map(|(index, rule, _)| {
                let app_id = rule.source.app_id.as_deref().filter(|value| !value.is_empty())?;
                let count = matches
                    .iter()
                    .filter(|(_, candidate, _)| candidate.source.app_id.as_deref() == Some(app_id))
                    .count();
                (count > 1).then_some((*index, app_id))
            })
            .min_by_key(|(index, _)| *index)
            .map(|(_, app_id)| app_id);

        let candidates = matches.iter().filter(|(_, rule, _)| {
            grouped_app_id.is_none_or(|app_id| rule.source.app_id.as_deref() == Some(app_id))
        });
        let best = candidates.max_by_key(|(index, _, specificity)| {
            (*specificity, std::cmp::Reverse(*index))
        });

        let Some((index, rule, _)) = best else {
            return RuleDecision::Managed {
                workspace: WorkspaceTarget::Current,
                floating: false,
                rule_index: None,
            };
        };
        if !rule.source.manage {
            RuleDecision::Unmanaged { rule_index: *index }
        } else {
            RuleDecision::Managed {
                workspace: rule.source.workspace.clone(),
                floating: rule.source.floating,
                rule_index: Some(*index),
            }
        }
    }
}

impl CompiledRule {
    fn matches(&self, identity: WindowIdentity<'_>) -> bool {
        matches_exact_ci(self.source.app_id.as_deref(), identity.app_id)
            && matches_name(self.source.app_name.as_deref(), identity.app_name)
            && matches_regex(self.source.title_regex.as_deref(), self.title_regex.as_ref(), identity.title)
            && matches_substring(self.source.title_substring.as_deref(), identity.title)
            && matches_exact(self.source.ax_role.as_deref(), identity.ax_role)
            && matches_exact(self.source.ax_subrole.as_deref(), identity.ax_subrole)
    }

    fn specificity(&self) -> usize {
        [
            self.source.app_id.as_deref(),
            self.source.app_name.as_deref(),
            self.source.title_regex.as_deref(),
            self.source.title_substring.as_deref(),
            self.source.ax_role.as_deref(),
            self.source.ax_subrole.as_deref(),
        ]
        .into_iter()
        .filter(|value| value.is_some_and(|value| !value.is_empty()))
        .count()
    }
}

fn matches_exact(expected: Option<&str>, actual: Option<&str>) -> bool {
    expected.is_none_or(|expected| !expected.is_empty() && actual == Some(expected))
}

fn matches_exact_ci(expected: Option<&str>, actual: Option<&str>) -> bool {
    expected.is_none_or(|expected| {
        !expected.is_empty()
            && actual.is_some_and(|actual| expected.eq_ignore_ascii_case(actual))
    })
}

fn matches_name(expected: Option<&str>, actual: Option<&str>) -> bool {
    expected.is_none_or(|expected| {
        !expected.is_empty()
            && actual.is_some_and(|actual| {
                let expected = expected.to_lowercase();
                let actual = actual.to_lowercase();
                actual.contains(&expected) || expected.contains(&actual)
            })
    })
}

fn matches_regex(
    source: Option<&str>,
    compiled: Option<&Regex>,
    actual: Option<&str>,
) -> bool {
    source.is_none_or(|source| {
        !source.is_empty()
            && compiled.is_some_and(|regex| actual.is_some_and(|actual| regex.is_match(actual)))
    })
}

fn matches_substring(expected: Option<&str>, actual: Option<&str>) -> bool {
    expected.is_none_or(|expected| {
        !expected.is_empty()
            && actual.is_some_and(|actual| actual.to_lowercase().contains(&expected.to_lowercase()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(value: u8) -> WorkspaceNumber {
        WorkspaceNumber::try_from(value).unwrap()
    }

    fn rule() -> WindowRule {
        WindowRule {
            app_id: None,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
            workspace: WorkspaceTarget::Current,
            floating: false,
            manage: true,
        }
    }

    #[test]
    fn most_specific_rule_wins_and_earlier_rule_breaks_ties() {
        let mut broad = rule();
        broad.app_id = Some("com.example.Editor".into());
        broad.workspace = WorkspaceTarget::Number(number(2));
        let mut specific = broad.clone();
        specific.title_substring = Some("settings".into());
        specific.workspace = WorkspaceTarget::Number(number(3));
        let mut tied_later = specific.clone();
        tied_later.workspace = WorkspaceTarget::Number(number(4));
        let rules = RuleSet::compile(vec![broad, specific, tied_later]).unwrap();

        assert_eq!(
            rules.decide(WindowIdentity {
                app_id: Some("COM.EXAMPLE.EDITOR"),
                title: Some("Project Settings"),
                ..WindowIdentity::default()
            }),
            RuleDecision::Managed {
                workspace: WorkspaceTarget::Number(number(3)),
                floating: false,
                rule_index: Some(1),
            }
        );
    }

    #[test]
    fn unmanaged_rule_is_explicit_and_invalid_regex_is_rejected() {
        let mut unmanaged = rule();
        unmanaged.app_name = Some("helper".into());
        unmanaged.manage = false;
        let rules = RuleSet::compile(vec![unmanaged]).unwrap();
        assert_eq!(
            rules.decide(WindowIdentity {
                app_name: Some("Helper App"),
                ..WindowIdentity::default()
            }),
            RuleDecision::Unmanaged { rule_index: 0 }
        );

        let mut invalid = rule();
        invalid.title_regex = Some("[".into());
        assert!(RuleSet::compile(vec![invalid]).is_err());
    }
}
