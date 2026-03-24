use tracing::info;

use crate::{
    Lsdb, Rid, Timeline, Update,
    ordering::{Error, ForwardType, forward_ordering},
};

pub fn nested_forward_ordering(lsdb: &Lsdb, root: Rid, update: &Update) -> Result<Timeline, Error> {
    info!("computing nested forward ordering");

    forward_ordering(lsdb, root, update, ForwardType::FullyNested, None, false)
}
