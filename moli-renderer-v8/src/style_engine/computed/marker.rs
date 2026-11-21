use style::{
    counter_style::CounterStyle,
    properties::ComputedValues,
    values::computed::{Content, ContentItem, Image},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerStyleDecision {
    Suppressed,
    ExplicitContent,
    DefaultListStyle,
}

/// The marker's content takes precedence over the originating element's list
/// style. A specified image is a candidate here; its resource isn't read by
/// style observation or box construction.
pub(crate) fn marker_style_decision(
    owner: &ComputedValues,
    marker: &ComputedValues,
) -> MarkerStyleDecision {
    use MarkerStyleDecision::{DefaultListStyle, ExplicitContent, Suppressed};

    let display = marker.clone_display();
    if !owner.clone_display().is_list_item() || display.is_none() || display.is_contents() {
        return Suppressed;
    }
    match &marker.get_counters().content {
        Content::None => Suppressed,
        // Like Chromium 156, `::marker { content: '' }` generates no marker
        // box: the list item's text starts where it would with `content: none`.
        Content::Items(items)
            if items.items[..items.alt_start]
                .iter()
                .all(|item| matches!(item, ContentItem::String(text) if text.is_empty())) =>
        {
            Suppressed
        }
        Content::Items(_) => ExplicitContent,
        Content::Normal => {
            let list = owner.get_list();
            if matches!(list.list_style_type.0, CounterStyle::None)
                && matches!(list.list_style_image, Image::None)
            {
                Suppressed
            } else {
                DefaultListStyle
            }
        }
    }
}
