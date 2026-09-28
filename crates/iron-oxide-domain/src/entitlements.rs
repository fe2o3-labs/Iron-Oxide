//! Plans and feature gating (#21): the one place that decides what each plan may do.
//!
//! Every gate in the app goes through [`allows`] (on/off features) or [`limit`] and
//! [`can_add`] (countable quotas). Nothing else looks at a [`Plan`] to decide what a user may do,
//! so changing the policy is a change to this file only, and a user whose `users.plan` flips gets
//! the new entitlements on their next request, with no other code change.
//!
//! The policy table, and what is never gated, are in `docs/billing.md`.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// A user's subscription plan (`users.plan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    /// The default for every new account.
    Free,
    /// The paid plan.
    Pro,
}

impl Plan {
    /// Every plan, cheapest first.
    pub const ALL: [Self; 2] = [Self::Free, Self::Pro];

    /// The stored and serialized name (`free`, `pro`), as in the `user_plan` database enum.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Pro => "pro",
        }
    }

    /// The name shown to users.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Pro => "Pro",
        }
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A plan name that is neither `free` nor `pro`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown plan")]
pub struct UnknownPlan;

impl FromStr for Plan {
    type Err = UnknownPlan;

    /// Parses the stored name, exactly (`free`, `pro`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "free" => Ok(Self::Free),
            "pro" => Ok(Self::Pro),
            _ => Err(UnknownPlan),
        }
    }
}

/// An on/off feature that a plan may or may not include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// Uploading your own `program.json` (#19, #35). Copying a built-in is not behind a feature,
    /// but the copy counts toward [`Quota::CustomPrograms`] like an upload.
    UploadPrograms,
    /// Per-exercise progress charts in the history (#33). The session list is never gated.
    ExerciseCharts,
}

impl Feature {
    /// Every feature.
    pub const ALL: [Self; 2] = [Self::UploadPrograms, Self::ExerciseCharts];
}

/// Something countable that a plan caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quota {
    /// Programs the user owns and has not archived: uploaded ones and copies of built-ins alike.
    /// Programs are archived, never deleted, so archiving one frees a slot and unarchiving one
    /// takes a slot.
    CustomPrograms,
}

impl Quota {
    /// Every quota.
    pub const ALL: [Self; 1] = [Self::CustomPrograms];
}

/// How many custom programs a free account may keep unarchived.
pub const FREE_CUSTOM_PROGRAMS: u32 = 10;

/// The cap a plan puts on a [`Quota`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Limit {
    /// No cap (abuse is bounded by rate limits, #23, not by the plan).
    Unlimited,
    /// At most `max` at a time.
    AtMost { max: u32 },
}

impl Limit {
    /// Whether one more is allowed when `used` are already taken.
    #[must_use]
    pub const fn allows_one_more(self, used: u32) -> bool {
        match self {
            Self::Unlimited => true,
            Self::AtMost { max } => used < max,
        }
    }
}

/// Whether `plan` includes `feature`. The single source of truth for on/off gates.
///
/// Both matches are exhaustive on purpose: adding a plan or a feature does not compile until
/// its row of the policy is written here.
#[must_use]
pub const fn allows(plan: Plan, feature: Feature) -> bool {
    match feature {
        // Everything is on the free plan for now (#21). Gate a feature by returning
        // `matches!(plan, Plan::Pro)`, and update the table in docs/billing.md.
        Feature::UploadPrograms | Feature::ExerciseCharts => match plan {
            Plan::Free | Plan::Pro => true,
        },
    }
}

/// The cap `plan` puts on `quota`. The single source of truth for countable gates.
#[must_use]
pub const fn limit(plan: Plan, quota: Quota) -> Limit {
    match (quota, plan) {
        (Quota::CustomPrograms, Plan::Free) => Limit::AtMost {
            max: FREE_CUSTOM_PROGRAMS,
        },
        (Quota::CustomPrograms, Plan::Pro) => Limit::Unlimited,
    }
}

/// Whether a user on `plan` who already has `used` of `quota` may add one more.
#[must_use]
pub const fn can_add(plan: Plan, quota: Quota, used: u32) -> bool {
    limit(plan, quota).allows_one_more(used)
}

/// Whether a feature is included, as sent to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureAccess {
    pub feature: Feature,
    pub allowed: bool,
}

/// A quota's cap, as sent to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaLimit {
    pub quota: Quota,
    pub limit: Limit,
}

/// Everything a plan includes: what the UI shows (locks, remaining slots, upgrade prompts).
///
/// It is derived from the plan by [`Entitlements::of`], never stored. The server still checks
/// every gate itself; the client's copy is for display only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entitlements {
    pub plan: Plan,
    /// One entry per [`Feature`], in [`Feature::ALL`] order.
    pub features: Vec<FeatureAccess>,
    /// One entry per [`Quota`], in [`Quota::ALL`] order.
    pub limits: Vec<QuotaLimit>,
}

impl Entitlements {
    /// What `plan` includes, from [`allows`] and [`limit`].
    #[must_use]
    pub fn of(plan: Plan) -> Self {
        Self {
            plan,
            features: Feature::ALL
                .iter()
                .map(|&feature| FeatureAccess {
                    feature,
                    allowed: allows(plan, feature),
                })
                .collect(),
            limits: Quota::ALL
                .iter()
                .map(|&quota| QuotaLimit {
                    quota,
                    limit: limit(plan, quota),
                })
                .collect(),
        }
    }

    /// Whether `feature` is included. A feature missing from the list (sent by an older server)
    /// counts as not included.
    #[must_use]
    pub fn allows(&self, feature: Feature) -> bool {
        self.features
            .iter()
            .any(|access| access.feature == feature && access.allowed)
    }

    /// The cap on `quota`, or `None` if the list does not mention it (an older server).
    #[must_use]
    pub fn limit(&self, quota: Quota) -> Option<Limit> {
        self.limits
            .iter()
            .find(|entry| entry.quota == quota)
            .map(|entry| entry.limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_names_round_trip_and_match_the_database_enum() {
        for plan in Plan::ALL {
            assert_eq!(plan.as_str().parse::<Plan>(), Ok(plan));
            assert_eq!(
                serde_json::to_string(&plan).unwrap(),
                format!("\"{}\"", plan.as_str())
            );
        }
        assert_eq!(Plan::Free.as_str(), "free");
        assert_eq!(Plan::Pro.as_str(), "pro");
    }

    #[test]
    fn unknown_plan_names_are_rejected() {
        for name in ["", "Free", "PRO", " pro", "enterprise", "free\0"] {
            assert_eq!(name.parse::<Plan>(), Err(UnknownPlan), "{name:?}");
        }
        assert!(serde_json::from_str::<Plan>("\"team\"").is_err());
    }

    #[test]
    fn plans_display_their_label() {
        assert_eq!(Plan::Free.to_string(), "Free");
        assert_eq!(Plan::Pro.to_string(), "Pro");
    }

    /// The whole on/off policy. Keep it in sync with the table in docs/billing.md.
    #[test]
    fn feature_matrix() {
        let expected = [
            (Plan::Free, Feature::UploadPrograms, true),
            (Plan::Free, Feature::ExerciseCharts, true),
            (Plan::Pro, Feature::UploadPrograms, true),
            (Plan::Pro, Feature::ExerciseCharts, true),
        ];
        assert_eq!(expected.len(), Plan::ALL.len() * Feature::ALL.len());
        for (plan, feature, allowed) in expected {
            assert_eq!(allows(plan, feature), allowed, "{plan:?} {feature:?}");
        }
    }

    /// The whole quota policy. Keep it in sync with the table in docs/billing.md.
    #[test]
    fn quota_matrix() {
        let expected = [
            (Plan::Free, Quota::CustomPrograms, Limit::AtMost { max: 10 }),
            (Plan::Pro, Quota::CustomPrograms, Limit::Unlimited),
        ];
        assert_eq!(expected.len(), Plan::ALL.len() * Quota::ALL.len());
        for (plan, quota, cap) in expected {
            assert_eq!(limit(plan, quota), cap, "{plan:?} {quota:?}");
        }
    }

    #[test]
    fn pro_never_has_less_than_free() {
        for feature in Feature::ALL {
            assert!(
                !allows(Plan::Free, feature) || allows(Plan::Pro, feature),
                "{feature:?}"
            );
        }
        for quota in Quota::ALL {
            for used in [0, FREE_CUSTOM_PROGRAMS - 1, FREE_CUSTOM_PROGRAMS, u32::MAX] {
                assert!(
                    !can_add(Plan::Free, quota, used) || can_add(Plan::Pro, quota, used),
                    "{quota:?} {used}"
                );
            }
        }
    }

    #[test]
    fn free_custom_programs_stop_at_the_limit() {
        let q = Quota::CustomPrograms;
        assert!(can_add(Plan::Free, q, 0));
        assert!(can_add(Plan::Free, q, FREE_CUSTOM_PROGRAMS - 1));
        assert!(!can_add(Plan::Free, q, FREE_CUSTOM_PROGRAMS));
        assert!(!can_add(Plan::Free, q, FREE_CUSTOM_PROGRAMS + 1));
        assert!(!can_add(Plan::Free, q, u32::MAX));
    }

    #[test]
    fn pro_custom_programs_are_unlimited() {
        for used in [0, FREE_CUSTOM_PROGRAMS, u32::MAX] {
            assert!(can_add(Plan::Pro, Quota::CustomPrograms, used), "{used}");
        }
    }

    #[test]
    fn limit_boundaries() {
        assert!(Limit::Unlimited.allows_one_more(u32::MAX));
        assert!(!Limit::AtMost { max: 0 }.allows_one_more(0));
        assert!(Limit::AtMost { max: 1 }.allows_one_more(0));
        assert!(!Limit::AtMost { max: 1 }.allows_one_more(1));
    }

    #[test]
    fn entitlements_list_every_gate_of_the_plan() {
        for plan in Plan::ALL {
            let e = Entitlements::of(plan);
            assert_eq!(e.plan, plan);
            let features: Vec<_> = e.features.iter().map(|a| a.feature).collect();
            assert_eq!(features, Feature::ALL);
            let quotas: Vec<_> = e.limits.iter().map(|l| l.quota).collect();
            assert_eq!(quotas, Quota::ALL);
            for feature in Feature::ALL {
                assert_eq!(e.allows(feature), allows(plan, feature), "{feature:?}");
            }
            for quota in Quota::ALL {
                assert_eq!(e.limit(quota), Some(limit(plan, quota)), "{quota:?}");
            }
        }
    }

    #[test]
    fn missing_entries_mean_not_included() {
        let e = Entitlements {
            plan: Plan::Pro,
            features: Vec::new(),
            limits: Vec::new(),
        };
        assert!(!e.allows(Feature::UploadPrograms));
        assert_eq!(e.limit(Quota::CustomPrograms), None);
        let denied = Entitlements {
            plan: Plan::Free,
            features: vec![FeatureAccess {
                feature: Feature::ExerciseCharts,
                allowed: false,
            }],
            limits: Vec::new(),
        };
        assert!(!denied.allows(Feature::ExerciseCharts));
    }

    #[test]
    fn wire_format_is_stable() {
        let json = serde_json::to_value(Entitlements::of(Plan::Free)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "plan": "free",
                "features": [
                    { "feature": "upload_programs", "allowed": true },
                    { "feature": "exercise_charts", "allowed": true },
                ],
                "limits": [
                    { "quota": "custom_programs", "limit": { "kind": "at_most", "max": 10 } },
                ],
            })
        );
        let pro = serde_json::to_value(Entitlements::of(Plan::Pro)).unwrap();
        assert_eq!(
            pro["limits"][0]["limit"],
            serde_json::json!({ "kind": "unlimited" })
        );
        let back: Entitlements = serde_json::from_value(json).unwrap();
        assert_eq!(back, Entitlements::of(Plan::Free));
    }
}
