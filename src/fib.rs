use std::collections::{HashMap, HashSet};

use tracing::{debug, debug_span, warn};

use crate::Rid;

pub type Fib = HashMap<Rid, (u64, Vec<Rid>)>;

#[derive(Debug, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[allow(dead_code)]
pub enum FibMetricChange {
    Increment { old: u64, new: u64 },
    Decrement { old: u64, new: u64 },
}

#[derive(Debug, Eq, PartialEq, PartialOrd, Ord, Hash, Clone)]
#[allow(dead_code)]
pub enum FibNxhChange {
    Removal(Rid),
    Addition(Rid),
    Swap { old: Rid, new: Rid },
}

#[derive(Debug, Eq, PartialEq, PartialOrd, Ord, Hash, Clone)]
#[allow(dead_code)]
pub enum FibChangeType {
    DestinationAddition,
    DestinationRemoval,
    MetricChange(FibMetricChange),
    NxhChange(FibNxhChange),
}

#[derive(Debug, Eq, PartialEq, Clone)]
#[allow(dead_code)]
pub struct FibChange {
    pub rid: Rid,
    pub dst: Rid,
    pub changes: HashSet<FibChangeType>,
}

pub fn compare_fibs(previous: &Fib, current: &Fib, rid: &Rid) -> Vec<FibChange> {
    let _span = debug_span!("fib analysis").entered();
    let previous_destinations: HashSet<&u32> = previous.keys().collect();
    let current_destinations: HashSet<&u32> = current.keys().collect();
    let mut changes: Vec<FibChange> = Default::default();

    // List lost destinations.
    let lost_destinations: HashSet<&Rid> = previous_destinations
        .difference(&current_destinations)
        .copied()
        .collect();
    if !lost_destinations.is_empty() {
        warn!(dst=?lost_destinations, "lost destinations");
        for &dst in lost_destinations {
            changes.push(FibChange {
                rid: *rid,
                dst,
                changes: HashSet::from_iter(vec![FibChangeType::DestinationRemoval]),
            })
        }
    }

    // List added destinations.
    let added_destinations: HashSet<&Rid> = current_destinations
        .difference(&previous_destinations)
        .copied()
        .collect();
    if !added_destinations.is_empty() {
        warn!(dst=?added_destinations, "added destinations");
        for &dst in added_destinations {
            changes.push(FibChange {
                rid: *rid,
                dst,
                changes: HashSet::from_iter(vec![FibChangeType::DestinationAddition]),
            })
        }
    }

    // Compare nexthops for a given destination.
    let _ = previous_destinations
        .intersection(&current_destinations)
        .into_iter()
        .for_each(|dst| {
            if let Some((previous_cost, previous_nxhs)) = previous.get(dst)
                && let Some((current_cost, current_nxhs)) = current.get(dst)
            {
                let previous_nxhs: HashSet<&Rid> = previous_nxhs.iter().collect();
                let current_nxhs: HashSet<&Rid> = current_nxhs.iter().collect();

                // Compute lost nexthops for a given destination.
                let lost_nexthops: Vec<&Rid> =
                    previous_nxhs.difference(&current_nxhs).copied().collect();

                // Compute added nexthops for a given destination.
                let added_nexthops: Vec<&Rid> =
                    current_nxhs.difference(&previous_nxhs).copied().collect();

                let cost_change =
                    (previous_cost != current_cost).then_some(if previous_cost > current_cost {
                        FibMetricChange::Decrement {
                            old: *previous_cost,
                            new: *current_cost,
                        }
                    } else {
                        FibMetricChange::Increment {
                            old: *previous_cost,
                            new: *current_cost,
                        }
                    });

                let mut nexthops_changes: HashSet<FibChangeType> = Default::default();
                if let Some(cost_change) = cost_change {
                    for nxh in current_nxhs {
                        debug!(
                            ?dst,
                            ?nxh,
                            old = previous_cost,
                            cur = current_cost,
                            "cost changed"
                        );
                        if current_cost >= &65534 {
                            debug!(?dst, "virtually removed");
                        }
                    }
                    nexthops_changes.insert(FibChangeType::MetricChange(cost_change.clone()));
                }

                if lost_nexthops.len() == added_nexthops.len() {
                    // Nxh swap
                    lost_nexthops
                        .iter()
                        .zip(added_nexthops)
                        .for_each(|(&&old, &new)| {
                            debug!(?dst, ?old, ?new, "nexthop swap");
                            nexthops_changes
                                .insert(FibChangeType::NxhChange(FibNxhChange::Swap { old, new }));
                        });
                } else {
                    // Nxh change
                    if !lost_nexthops.is_empty() {
                        lost_nexthops.iter().for_each(|&&nxh| {
                            debug!(?nxh, "nexthop lost");
                            nexthops_changes
                                .insert(FibChangeType::NxhChange(FibNxhChange::Removal(nxh)));
                        });
                    }

                    if !added_nexthops.is_empty() {
                        added_nexthops.iter().for_each(|&&nxh| {
                            debug!(?nxh, "nexthop added");
                            nexthops_changes
                                .insert(FibChangeType::NxhChange(FibNxhChange::Addition(nxh)));
                        });
                    }
                }
                if !nexthops_changes.is_empty() {
                    changes.push(FibChange {
                        rid: *rid,
                        dst: **dst,
                        changes: nexthops_changes,
                    });
                }
            }
        });

    changes
}

#[cfg(test)]
mod compare_fibs {
    use super::*;

    fn compare(previous: &Fib, current: &Fib, rid: &Rid, expected: Vec<FibChange>) {
        let got = compare_fibs(previous, current, rid);
        assert_eq!(got.len(), expected.len());
        expected.iter().for_each(|e| assert!(got.contains(e)));
    }

    mod house3 {
        use super::*;

        fn house_3_fibs() -> HashMap<Rid, Fib> {
            let mut fibs: HashMap<Rid, Fib> = Default::default();

            fibs.insert(
                1,
                HashMap::from_iter(vec![
                    (2, (1, vec![2])),
                    (3, (1, vec![3])),
                    (4, (2, vec![2])),
                    (5, (2, vec![3])),
                ]),
            );

            fibs.insert(
                2,
                HashMap::from_iter(vec![
                    (1, (1, vec![1])),
                    (3, (3, vec![3, 4])),
                    (4, (1, vec![4])),
                    (5, (2, vec![4])),
                ]),
            );

            fibs.insert(
                3,
                HashMap::from_iter(vec![
                    (1, (1, vec![1])),
                    (2, (3, vec![2, 5])),
                    (4, (2, vec![5])),
                    (5, (1, vec![5])),
                ]),
            );

            fibs.insert(
                4,
                HashMap::from_iter(vec![
                    (1, (2, vec![2])),
                    (2, (1, vec![1])),
                    (3, (2, vec![5])),
                    (5, (1, vec![5])),
                ]),
            );

            fibs
        }

        fn house_3_fibs_minus13() -> HashMap<Rid, Fib> {
            let mut fibs: HashMap<Rid, Fib> = Default::default();

            fibs.insert(
                1,
                HashMap::from_iter(vec![
                    (2, (1, vec![2])),
                    (3, (4, vec![2])),
                    (4, (2, vec![2])),
                    (5, (2, vec![3])),
                ]),
            );

            fibs.insert(
                2,
                HashMap::from_iter(vec![
                    (1, (1, vec![1])),
                    (3, (3, vec![3, 4])),
                    (4, (1, vec![4])),
                    (5, (2, vec![4])),
                ]),
            );

            fibs.insert(
                3,
                HashMap::from_iter(vec![
                    (1, (4, vec![2, 5])),
                    (2, (3, vec![2, 5])),
                    (4, (2, vec![5])),
                    (5, (1, vec![5])),
                ]),
            );

            fibs.insert(
                4,
                HashMap::from_iter(vec![
                    (1, (2, vec![2])),
                    (2, (1, vec![1])),
                    (3, (2, vec![5])),
                    (5, (1, vec![5])),
                ]),
            );

            fibs
        }

        fn compare_change(rid: &Rid, expected: Vec<FibChange>) {
            let previous = house_3_fibs()[rid].clone();
            let current = house_3_fibs_minus13()[rid].clone();
            compare(&previous, &current, rid, expected);
        }

        #[test]
        // Test nexthop swap and metric change.
        fn test_node_1() {
            let rid = 1;
            let expected: Vec<FibChange> = vec![FibChange {
                rid,
                dst: 3,
                changes: HashSet::from_iter(vec![
                    FibChangeType::MetricChange(FibMetricChange::Increment { old: 1, new: 4 }),
                    FibChangeType::NxhChange(FibNxhChange::Swap { old: 3, new: 2 }),
                ]),
            }];
            compare_change(&rid, expected);
        }

        #[test]
        // Test no change.
        fn test_node_2() {
            let rid = 2;
            compare_change(&rid, Default::default());
        }

        #[test]
        // Test metric change, nextop removal and nexthop addition.
        fn test_node_3() {
            let rid = 3;
            let expected: Vec<FibChange> = vec![FibChange {
                rid,
                dst: 1,
                changes: HashSet::from_iter(vec![
                    FibChangeType::MetricChange(FibMetricChange::Increment { old: 1, new: 4 }),
                    FibChangeType::NxhChange(FibNxhChange::Removal(1)),
                    FibChangeType::NxhChange(FibNxhChange::Addition(2)),
                    FibChangeType::NxhChange(FibNxhChange::Addition(5)),
                ]),
            }];
            compare_change(&rid, expected);
        }

        #[test]
        // Test no change.
        fn test_node_4() {
            let rid = 4;
            let expected: Vec<FibChange> = Default::default();
            compare_change(&rid, expected);
        }

        // 5 is not tested as it is symmetric to 4.
    }
}
