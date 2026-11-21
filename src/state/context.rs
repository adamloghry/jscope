//! A frozen neighborhood in journal time. Replay delivery time can differ by hours.
use super::{Event, State};
use std::{collections::BTreeMap, sync::Arc};

const RADIUS_US: u64 = 2_000_000;
const MAX_ENTRIES: usize = 257;

#[derive(Debug)]
pub struct Surroundings {
    pub events: Vec<Arc<Event>>,
    /// Position of the occurrence that opened this view.
    pub anchor: usize,
    pub selected: usize,
    /// Other matching, retained receipts beyond the context view's size limit.
    pub omitted: usize,
}

impl State {
    pub fn context(&self) -> Option<&Surroundings> {
        self.context.as_ref()
    }

    pub fn open_context(&mut self) -> Result<(), &'static str> {
        let (id, event) = if let Some(spike) = self.spike() {
            let (rank, position) = spike.following.ok_or("Select a message occurrence first")?;
            let target = &spike.bins[spike.selected].drivers[rank].receipts[position];
            (target.receipt_id, &target.event)
        } else if let Some(hit) = &self.spike_hit {
            (hit.receipt_id, &hit.event)
        } else {
            let row = self.selected_group().ok_or("No selected journal entry")?;
            (row.id, &row.first)
        };
        let center = event
            .realtime_us
            .ok_or("Context requires a journal timestamp")?;
        // One receipt can be held by several buffers. Use its intake sequence both
        // to deduplicate and to order distinct receipts with the same timestamp.
        let mut entries = BTreeMap::new();
        let mut add = |id, event: &Arc<Event>| {
            if let Some(us) = event.realtime_us
                && us.abs_diff(center) <= RADIUS_US
            {
                entries.entry((us, id)).or_insert_with(|| Arc::clone(event));
            }
        };
        add(id, event);
        for row in &self.rows {
            add(row.id, &row.first);
            add(row.last_id, &row.last);
        }
        for entry in &self.intake {
            add(entry.first_id, &entry.event);
        }
        for receipt in &self.repeats {
            add(receipt.receipt_id, &receipt.event);
        }
        if let Some(spike) = &self.spike {
            let bins = spike
                .bins
                .iter()
                .chain(spike.peer.iter().flat_map(|lane| &lane.bins));
            for bin in bins {
                for receipt in bin.target.iter().chain(
                    bin.drivers
                        .iter()
                        .chain(&bin.alternate_drivers)
                        .flat_map(|d| &d.receipts),
                ) {
                    add(receipt.receipt_id, &receipt.event);
                }
            }
        }
        if let Some(trail) = &self.spike_trail {
            for receipt in trail.targets.iter().flatten() {
                add(receipt.receipt_id, &receipt.event);
            }
        }
        let anchor = entries.range(..(center, id)).count();
        let omitted = entries.len().saturating_sub(MAX_ENTRIES);
        let start = anchor.saturating_sub(MAX_ENTRIES / 2).min(omitted);
        let anchor = anchor - start;
        self.context = Some(Surroundings {
            events: entries
                .into_values()
                .skip(start)
                .take(MAX_ENTRIES)
                .collect(),
            anchor,
            selected: anchor,
            omitted,
        });
        Ok(())
    }

    pub fn move_context(&mut self, delta: isize) {
        if let Some(context) = &mut self.context {
            context.selected = context
                .selected
                .saturating_add_signed(delta)
                .min(context.events.len() - 1);
        }
    }

    pub fn close_context(&mut self) {
        self.context = None;
    }
}
