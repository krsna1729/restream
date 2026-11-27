use super::super::*;
use crate::config::EgressFabricConfig;
use std::time::Duration;

/// Every fabric setting reaches its own shard setting with its own unit; a
/// swapped pair of budgets or a wrong `Duration` unit fails here.
#[test]
fn shard_config_from_fabric_maps_each_field_with_its_unit() {
    let fabric = EgressFabricConfig {
        command_channel_capacity: 101,
        command_batch_budget: 102,
        readiness_batch_budget: 103,
        timer_batch_budget: 104,
        max_leaves_per_shard: 105,
        idle_wait_ms: 106,
        drain_timeout_ms: 107,
        ..EgressFabricConfig::default()
    };
    let config = EgressShardConfig::from_fabric(&fabric);
    assert_eq!(config.command_channel_capacity().get(), 101);
    assert_eq!(config.command_batch_budget().get(), 102);
    assert_eq!(config.readiness_batch_budget().get(), 103);
    assert_eq!(config.timer_batch_budget().get(), 104);
    assert_eq!(config.leaf_capacity().get(), 105);
    assert_eq!(config.idle_wait(), Duration::from_millis(106));
    assert_eq!(config.drain_timeout(), Duration::from_millis(107));
}
