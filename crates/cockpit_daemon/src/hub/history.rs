//! Pages of an agent's transcript, newest first, bounded by count and by bytes.

use std::collections::BTreeMap;

use cockpit_protocol::{
    HistoryPage, ItemBody, ItemId, MAX_LINE_BYTES, NoticeKind, OlderItems, TranscriptItem,
};

/// The most items one page holds, whatever the client asks.
const MAX_PAGE_ITEMS: usize = 500;
/// The most bytes of items one page holds, well under the framing maximum so the response with
/// its envelope always fits. Images travel inline, so a few items can be large.
const MAX_PAGE_BYTES: usize = MAX_LINE_BYTES / 2;
/// What a history response adds around its items, with room to spare.
const RESPONSE_ENVELOPE_BYTES: usize = 1024;
/// The largest item a page sends as it is. A larger one could never fit in a response, so a
/// placeholder takes its place and paging moves on.
const MAX_ITEM_BYTES: usize = MAX_LINE_BYTES - RESPONSE_ENVELOPE_BYTES;

/// The byte limits of a page.
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// The items of a page together. A page always holds one item, whatever its size.
    page_bytes: usize,
    /// One item, past which a placeholder replaces it.
    item_bytes: usize,
}

/// The items before `before` (or the latest ones), oldest first.
pub(super) fn page(items: &[TranscriptItem], before: Option<ItemId>, limit: u16) -> HistoryPage {
    let limits = Limits {
        page_bytes: MAX_PAGE_BYTES,
        item_bytes: MAX_ITEM_BYTES,
    };
    page_within(items, before, limit, limits)
}

fn page_within(
    items: &[TranscriptItem],
    before: Option<ItemId>,
    limit: u16,
    limits: Limits,
) -> HistoryPage {
    let end = match before {
        Some(before) => items.partition_point(|item| item.id < before),
        None => items.len(),
    };
    let limit = usize::from(limit).min(MAX_PAGE_ITEMS);
    let mut start = end;
    let mut bytes: usize = 0;
    // Newest first, reversed at the end.
    let mut page: Vec<TranscriptItem> = Vec::new();
    while page.len() < limit {
        let Some(item) = start.checked_sub(1).and_then(|index| items.get(index)) else {
            break;
        };
        let encoded = encoded_size(item);
        let stand_in = (encoded > limits.item_bytes).then(|| placeholder(item, encoded));
        let size = stand_in.as_ref().map_or(encoded, encoded_size);
        if !page.is_empty() && bytes.saturating_add(size) > limits.page_bytes {
            break;
        }
        bytes = bytes.saturating_add(size);
        start -= 1;
        page.push(stand_in.unwrap_or_else(|| item.clone()));
    }
    page.reverse();
    let older = items.get(..start).unwrap_or_default();
    let mut ancestors = Vec::new();
    for ancestor in ancestors_of(older, &page) {
        let encoded = encoded_size(ancestor);
        let stand_in = (encoded > limits.item_bytes).then(|| placeholder(ancestor, encoded));
        let size = stand_in.as_ref().map_or(encoded, encoded_size);
        if bytes.saturating_add(size) > limits.page_bytes {
            break;
        }
        bytes = bytes.saturating_add(size);
        ancestors.push(stand_in.unwrap_or_else(|| ancestor.clone()));
    }
    HistoryPage {
        items: page,
        older_items: if start > 0 {
            OlderItems::Exist
        } else {
            OlderItems::None
        },
        ancestors,
    }
}

/// The items of `older` that the page's items run under, at any depth, oldest first. `older`
/// is in id order, as the transcript is.
fn ancestors_of<'items>(
    older: &'items [TranscriptItem],
    page: &[TranscriptItem],
) -> Vec<&'items TranscriptItem> {
    let find = |id: ItemId| {
        older
            .binary_search_by_key(&id, |item| item.id)
            .ok()
            .and_then(|index| older.get(index))
    };
    let mut found: BTreeMap<ItemId, &TranscriptItem> = BTreeMap::new();
    let mut next: Vec<ItemId> = page.iter().filter_map(|item| item.parent).collect();
    while let Some(id) = next.pop() {
        if found.contains_key(&id) {
            continue;
        }
        // A parent in the page itself is not older: its own parent is in `next` already.
        let Some(item) = find(id) else {
            continue;
        };
        found.insert(id, item);
        next.extend(item.parent);
    }
    found.into_values().collect()
}

/// `usize::MAX` for an item that does not encode, which no page can hold.
fn encoded_size(item: &TranscriptItem) -> usize {
    serde_json::to_vec(item).map_or(usize::MAX, |encoded| encoded.len())
}

/// A notice in place of an item too large to send, under the same id and parent.
fn placeholder(item: &TranscriptItem, bytes: usize) -> TranscriptItem {
    TranscriptItem {
        id: item.id,
        parent: item.parent,
        body: ItemBody::Notice {
            text: format!("This item is too large to show ({bytes} bytes)."),
            detail: None,
            kind: NoticeKind::Plain,
        },
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{Frame, RequestId, ResponseBody, ResponseResult, ServerMessage};

    use super::*;

    fn notice(id: u32, text: String) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::Notice {
                text,
                detail: None,
                kind: NoticeKind::Plain,
            },
        }
    }

    fn items(count: u32) -> Vec<TranscriptItem> {
        (1..=count)
            .map(|id| notice(id, format!("item {id}")))
            .collect()
    }

    fn ids(page: &HistoryPage) -> Vec<u32> {
        page.items.iter().map(|item| item.id.get()).collect()
    }

    #[test]
    fn a_page_brings_the_subagent_calls_its_items_run_under() {
        // Two subagents started early, at 2 and 3; 4 runs under 3, and 10 to 12 under 4 and 2.
        let mut items = items(12);
        for (id, parent) in [(4, 3), (10, 4), (11, 2), (12, 4)] {
            if let Some(item) = items.get_mut(id - 1) {
                item.parent = Some(ItemId::new(parent));
            }
        }

        let page = page(&items, None, 3);
        let older = super::page(&items, Some(ItemId::new(10)), 3);

        assert_eq!(ids(&page), [10, 11, 12]);
        let ancestors: Vec<u32> = page.ancestors.iter().map(|item| item.id.get()).collect();
        assert_eq!(ancestors, [2, 3, 4], "every level, oldest first");
        assert_eq!(page.older_items, OlderItems::Exist);
        assert_eq!(ids(&older), [7, 8, 9]);
        assert!(older.ancestors.is_empty());
    }

    #[test]
    fn pages_walk_back_from_the_latest_items() {
        let items = items(5);

        let latest = page(&items, None, 2);
        let older = page(&items, Some(ItemId::new(4)), 2);
        let oldest = page(&items, Some(ItemId::new(2)), 2);

        assert_eq!(
            (ids(&latest), latest.older_items),
            (vec![4, 5], OlderItems::Exist)
        );
        assert_eq!(
            (ids(&older), older.older_items),
            (vec![2, 3], OlderItems::Exist)
        );
        assert_eq!(
            (ids(&oldest), oldest.older_items),
            (vec![1], OlderItems::None)
        );
    }

    #[test]
    fn a_page_stops_at_its_byte_budget_but_holds_one_item() {
        let items = items(5);
        let one_item = encoded_size(&items[0]);
        let limits = |page_bytes| Limits {
            page_bytes,
            item_bytes: MAX_ITEM_BYTES,
        };

        let page = page_within(&items, None, 10, limits(one_item * 2 + 1));
        let tiny = page_within(&items, None, 10, limits(1));

        assert_eq!(ids(&page), [4, 5]);
        assert_eq!(page.items, items[3..]);
        assert_eq!(ids(&tiny), [5]);
        assert_eq!(tiny.items, items[4..]);
    }

    #[test]
    fn an_item_too_large_to_send_becomes_a_placeholder_and_paging_moves_on() {
        let mut items = items(3);
        items[1] = notice(2, "x".repeat(500));
        let limits = Limits {
            page_bytes: MAX_PAGE_BYTES,
            item_bytes: 100,
        };

        let latest = page_within(&items, None, 1, limits);
        let large = page_within(&items, Some(ItemId::new(3)), 1, limits);
        let oldest = page_within(&items, Some(ItemId::new(2)), 1, limits);

        assert_eq!(latest.items, items[2..]);
        let ItemBody::Notice { text, .. } = &large.items[0].body else {
            panic!("expected a placeholder, got {:?}", large.items);
        };
        assert!(text.contains("too large"), "{text}");
        assert_eq!(
            (ids(&large), large.older_items),
            (vec![2], OlderItems::Exist)
        );
        assert_eq!(
            (ids(&oldest), oldest.older_items),
            (vec![1], OlderItems::None)
        );
    }

    /// The envelope's margin holds: a response with the largest item sent as it is still fits.
    #[test]
    fn a_page_with_the_largest_item_fits_in_a_response() {
        let empty_item = encoded_size(&notice(u32::MAX, String::new()));
        let largest = [notice(u32::MAX, "x".repeat(MAX_ITEM_BYTES - empty_item))];

        let sent = page(&largest, None, 1);

        assert_eq!(sent.items, largest);
        let response = ServerMessage::Response {
            id: RequestId::new(u32::MAX),
            result: ResponseResult::Ok(ResponseBody::History(sent)),
        };
        assert!(Frame::encode(&response).is_ok());
    }
}
