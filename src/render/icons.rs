//! Icons are glyphs of the system "Segoe Fluent Icons" font, so they stay sharp at any size.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Prev,
    Next,
    Shuffle,
    Repeat,
    Close,
    Timer,
    Stopwatch,
    Flag,
    Headphones,
    Cast,
    Speaker,
    Earbuds,
    Headset,
    Monitor,
    Volume,
    Mute,
    Check,
    Back,
    Settings,
    Sync,
    Pencil,
    Prompt,
    Search,
    People,
    Wrench,
    Document,
    Folder,
    Image,
    Video,
    Music,
    Pdf,
    Archive,
    TextDoc,
    Code,
    Download,
}

impl Icon {
    pub const ALL: [Icon; 37] = [
        Icon::Play,
        Icon::Pause,
        Icon::Prev,
        Icon::Next,
        Icon::Shuffle,
        Icon::Repeat,
        Icon::Close,
        Icon::Timer,
        Icon::Stopwatch,
        Icon::Flag,
        Icon::Headphones,
        Icon::Cast,
        Icon::Speaker,
        Icon::Earbuds,
        Icon::Headset,
        Icon::Monitor,
        Icon::Volume,
        Icon::Mute,
        Icon::Check,
        Icon::Back,
        Icon::Settings,
        Icon::Sync,
        Icon::Pencil,
        Icon::Prompt,
        Icon::Search,
        Icon::People,
        Icon::Wrench,
        Icon::Document,
        Icon::Folder,
        Icon::Image,
        Icon::Video,
        Icon::Music,
        Icon::Pdf,
        Icon::Archive,
        Icon::TextDoc,
        Icon::Code,
        Icon::Download,
    ];
}

/// Segoe Fluent Icons code point for an icon (checked visually with the `glyphsheet` tool).
/// Transport controls use the Solid variants, which match One UI's filled glyphs.
pub fn glyph(i: Icon) -> char {
    match i {
        Icon::Play => '\u{F5B0}',
        Icon::Pause => '\u{F8AE}',
        Icon::Prev => '\u{F8AC}',
        Icon::Next => '\u{F8AD}',
        Icon::Shuffle => '\u{E8B1}',
        Icon::Repeat => '\u{E8EE}',
        Icon::Close => '\u{E711}',
        Icon::Timer => '\u{E823}',
        Icon::Stopwatch => '\u{E916}',
        Icon::Flag => '\u{E7C1}',
        Icon::Headphones => '\u{E7F6}',
        Icon::Cast => '\u{EC15}',
        Icon::Speaker => '\u{E7F5}',
        Icon::Earbuds => '\u{F4C0}',
        Icon::Headset => '\u{E95B}',
        Icon::Monitor => '\u{E7F4}',
        Icon::Volume => '\u{E767}',
        Icon::Mute => '\u{E74F}',
        Icon::Check => '\u{E73E}',
        Icon::Back => '\u{E76B}',
        Icon::Settings => '\u{E713}',
        Icon::Sync => '\u{E895}',
        Icon::Pencil => '\u{E70F}',
        Icon::Prompt => '\u{E756}',
        Icon::Search => '\u{E721}',
        Icon::People => '\u{E716}',
        Icon::Wrench => '\u{E90F}',
        Icon::Document => '\u{E8A5}',
        Icon::Folder => '\u{E8B7}',
        Icon::Image => '\u{EB9F}',
        Icon::Video => '\u{E714}',
        Icon::Music => '\u{EC4F}',
        Icon::Pdf => '\u{EA90}',
        Icon::Archive => '\u{F012}',
        Icon::TextDoc => '\u{F000}',
        Icon::Code => '\u{E943}',
        Icon::Download => '\u{E896}',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_distinct() {
        let all = Icon::ALL;
        for (i, a) in all.iter().enumerate() {
            let g = glyph(*a) as u32;
            assert!((0xE000..=0xF8FF).contains(&g), "{a:?} outside the private use area");
            for b in &all[i + 1..] {
                assert_ne!(glyph(*a), glyph(*b), "{a:?} and {b:?} share a glyph");
            }
        }
    }
}
