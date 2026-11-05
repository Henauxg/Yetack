//! Pages of an agent's transcript, newest first, bounded by count and by bytes.

use cockpit_protocol::{HistoryPage, ItemId, MAX_LINE_BYTES, TranscriptItem};

/// The most items one page holds, whatever the client asks.
const MAX_PAGE_ITEMS: usize = 500;
/// The most bytes of items one page holds, well under the framing maximum so the response with
/// its envelope always fits. Images travel inline, so a few items can be large.
const MAX_PAGE_BYTES: usize = MAX_LINE_BYTES / 2;

/// The items before `before` (or the latest ones), oldest first.
pub(super) fn page(items: &[TranscriptItem], before: Option<ItemId>, limit: u16) -> HistoryPage {
    page_within(items, before, limit, MAX_PAGE_BYTES)
}

fn page_within(
    items: &[TranscriptItem],
    before: Option<ItemId>,
    limit: u16,
    max_bytes: usize,
) -> HistoryPage {
    let end = match before {
        Some(before) => items.partition_point(|item| item.id < before),
        None => items.len(),
    };
    let limit = usize::from(limit).min(MAX_PAGE_ITEMS);
    let mut start = end;
    let mut bytes: usize = 0;
    while end - start < limit {
        let Some(item) = start.checked_sub(1).and_then(|index| items.get(index)) else {
            break;
        };
        let size = serde_json::to_vec(item).map_or(usize::MAX, |encoded| encoded.len());
        // A page always holds one item, so that paging moves on.
        if start < end && bytes.saturating_add(size) > max_bytes {
            break;
        }
        bytes = bytes.saturating_add(size);
        start -= 1;
    }
    HistoryPage {
        items: items.get(start..end).unwrap_or_default().to_vec(),
        has_older: start > 0,
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::ItemBody;

    use super::*;

    fn items(count: u32) -> Vec<TranscriptItem> {
        (1..=count)
            .map(|id| TranscriptItem {
                id: ItemId::new(id),
                parent: None,
                body: ItemBody::Notice {
                    text: format!("item {id}"),
                },
            })
            .collect()
    }

    fn ids(page: &HistoryPage) -> Vec<u32> {
        page.items.iter().map(|item| item.id.get()).collect()
    }

    #[test]
    fn pages_walk_back_from_the_latest_items() {
        let items = items(5);

        let latest = page(&items, None, 2);
        let older = page(&items, Some(ItemId::new(4)), 2);
        let oldest = page(&items, Some(ItemId::new(2)), 2);

        assert_eq!((ids(&latest), latest.has_older), (vec![4, 5], true));
        assert_eq!((ids(&older), older.has_older), (vec![2, 3], true));
        assert_eq!((ids(&oldest), oldest.has_older), (vec![1], false));
    }

    #[test]
    fn a_page_stops_at_its_byte_budget_but_holds_one_item() {
        let items = items(5);
        let one_item = serde_json::to_vec(&items[0]).unwrap().len();

        let page = page_within(&items, None, 10, one_item * 2 + 1);
        let tiny = page_within(&items, None, 10, 1);

        assert_eq!(ids(&page), [4, 5]);
        assert_eq!(ids(&tiny), [5]);
    }
}
