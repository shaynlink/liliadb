//! Bitemporal entities: writes are protocol batches; reads come from the entity's handle.

use lilia_core::{BatchOperation, BitemporalInterval, BitemporalPoint, LiliaError};
use lilia_db::Database;
use serde_json::Value;

use super::operations::{self, Call};
use super::{input, render};
use crate::args::BitempCommand;

pub(crate) fn run(database: &Database, command: BitempCommand) -> Result<Value, LiliaError> {
    let session = database.system();
    match command {
        BitempCommand::Put {
            space,
            id,
            value,
            vt_from,
            vt_to,
            system_time,
        } => {
            let valid = BitemporalInterval::inclusive_days(&vt_from, &vt_to)?;
            operations::run(
                database,
                Call::Write(BatchOperation::BitemporalPut {
                    space,
                    id,
                    value: input::json(&value, "value")?,
                    vt_from: valid.from,
                    vt_to: valid.to,
                    system_time: system_time
                        .as_deref()
                        .map(lilia_core::parse_iso_to_ms)
                        .transpose()?,
                }),
            )
        }
        BitempCommand::Delete {
            space,
            id,
            vt_from,
            vt_to,
        } => {
            let valid = vt_from
                .zip(vt_to)
                .map(|(from, to)| BitemporalInterval::between(&from, &to))
                .transpose()?;
            operations::run(
                database,
                Call::Write(BatchOperation::BitemporalDelete {
                    space,
                    id,
                    vt_from: valid.map(|v| v.from),
                    vt_to: valid.map(|v| v.to),
                    system_time: None,
                }),
            )
        }
        BitempCommand::Get {
            space,
            id,
            as_of_tt,
            for_vt,
        } => render(
            &session
                .bitemporal(&space)
                .get_as_of(
                    &id,
                    BitemporalPoint::at(&as_of_tt)?,
                    BitemporalPoint::at(&for_vt)?,
                )
                .map_err(LiliaError::from)?,
        ),
        BitempCommand::Timeline {
            space,
            id,
            as_of_tt,
        } => render(
            &session
                .bitemporal(&space)
                .timeline_as_of(&id, BitemporalPoint::at(&as_of_tt)?)
                .map_err(LiliaError::from)?,
        ),
        BitempCommand::History { space, id } => render(
            &session
                .bitemporal(&space)
                .history(&id)
                .map_err(LiliaError::from)?,
        ),
    }
}
