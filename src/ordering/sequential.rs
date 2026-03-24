use tracing::info;

use crate::{
    Lsdb, Rid, Timeline, Timestamp, Update,
    lsdb::apply_update,
    ordering::{Error, ForwardType, forward_ordering, reverse_ordering},
};

pub fn sequential_ordering(
    lsdb: &Lsdb,
    forward_root: Rid,
    reverse_root: Rid,
    update: &Update,
    delay: Option<Timestamp>,
) -> Result<Timeline, Error> {
    info!("Applying sequential ordering.");
    let mut forward_update = update.clone();
    forward_update.reverse = Default::default();

    let mut reverse_update = update.clone();
    reverse_update.forward = Default::default();

    // First distribute and apply the forward update.
    if let Ok(forward_timeline) = forward_ordering(
        lsdb,
        forward_root,
        &forward_update,
        ForwardType::ForwardOrder,
        None,
        false,
    ) {
        // Compute the new LSDB after Forward update.
        let mut lsdb = lsdb.clone();
        apply_update(&mut lsdb, &update.forward);

        // Then distribute the reverse update.
        if let Ok(mut forward_reverse_timeline) = forward_ordering(
            &mut lsdb,
            reverse_root,
            &reverse_update,
            ForwardType::ForwardOnly,
            Some(forward_timeline),
            true,
        ) {
            // Then apply the reverse ordering.
            reverse_ordering(
                &mut lsdb,
                reverse_root,
                &reverse_update,
                delay,
                true,
                &mut forward_reverse_timeline,
            );
            Ok(forward_reverse_timeline)
        } else {
            Err(Error::A)
        }
    } else {
        Err(Error::A)
    }
}
