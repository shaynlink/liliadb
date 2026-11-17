pub mod eval;
pub mod template;
pub mod types;

pub use eval::{
    eval_rule_condition_json, eval_rule_condition_kv, eval_rule_condition_value,
    infer_target_collection,
};
pub use template::{interpolate_string, interpolate_value, RuleContext};
pub use types::{
    Action, Rule, RuleBuilder, SlidingWindow, TriggerMode, WindowCondition,
};

/// Convenient macro for constructing an in-transaction [`Rule`].
///
/// # Example
/// ```rust,ignore
/// use lilia_core::rule;
/// let r = rule! {
///     when: "events.heart_rate > 140 AND events.motion == 'still'",
///     then: Action::insert("emergency_alerts", doc! { "level": "critical", "created_at": "$now" }),
/// };
/// ```
#[macro_export]
macro_rules! rule {
    (when: $when:expr, then: $then:expr $(,)?) => {
        $crate::rules::Rule {
            when: $when.to_string(),
            window: None,
            then: $then,
            target: None,
        }
    };
    (when: $when:expr, window: $window:expr, then: $then:expr $(,)?) => {
        $crate::rules::Rule {
            when: $when.to_string(),
            window: Some($window),
            then: $then,
            target: None,
        }
    };
}
