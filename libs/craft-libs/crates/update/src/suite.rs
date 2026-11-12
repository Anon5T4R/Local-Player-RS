//! The Craft App Suite as releases know it: each app's id, name and repository.
//!
//! An app that updates itself takes its own entry (`suite::PHOTOCRAFT`); the launcher takes all
//! of them. Installers use the list to tell a Craft App's package from anyone else's.

use crate::app::App;

pub const PHOTOCRAFT: App = App::new("photocraft", "PhotoCraft", "Image editing", "storytold/photocraft");
pub const VECTORCRAFT: App = App::new("vectorcraft", "VectorCraft", "Vector illustration", "storytold/vectorcraft");
pub const FILMCRAFT: App = App::new("filmcraft", "FilmCraft", "Video editing, color and sound", "storytold/filmcraft");
pub const LIGHTCRAFT: App = App::new("lightcraft", "LightCraft", "Photo library and raw development", "storytold/lightcraft");
pub const PDFCRAFT: App = App {
    former_ids: &["printcraft"],
    former_names: &["PrintCraft"],
    ..App::new("pdfcraft", "PdfCraft", "Reading, organizing and protecting PDFs", "storytold/pdfcraft")
};
pub const EFFECTCRAFT: App = App::new("effectcraft", "EffectCraft", "Motion graphics and visual effects", "storytold/effectcraft");
pub const DESIGNCRAFT: App = App::new("designcraft", "DesignCraft", "Page layout and publishing", "storytold/designcraft");
pub const FIGCRAFT: App = App::new("figcraft", "FigCraft", "Interface design and prototyping", "storytold/figcraft");
pub const LOCALPLAYER: App =
    App { publisher: "LocalPlayer contributors", ..App::new("local-player", "Local Player", "Media player", "Anon5T4R/Local-Player-RS") };
pub const SOUNDCRAFT: App = App::new("soundcraft", "SoundCraft", "Recording, editing and mixing audio", "storytold/soundcraft");
pub const DECKCRAFT: App = App::new("deckcraft", "DeckCraft", "Presentations and slide shows", "storytold/deckcraft");
pub const LOCALSLIDES: App = App {
    publisher: "Local Slides contributors",
    ..App::new("localslides", "Local Slides", "Slide decks and presentations", "Anon5T4R/local-slides")
};
pub const LOCALSHEETS: App =
    App { publisher: "Anon5T4R", ..App::new("localsheets", "Local Sheets", "Spreadsheets and data", "Anon5T4R/local-sheets") };
pub const CADCRAFT: App = App::new("cadcraft", "CADCraft", "Computer-aided design and drafting", "storytold/cadcraft");
pub const GRIDCRAFT: App = App::new("gridcraft", "GridCraft", "Spreadsheets and data", "storytold/gridcraft");

/// ArtCraft Launcher, described like a Craft App so it updates itself with the same machinery
/// (asset names `artcraft-launcher-<ver>-…`, bundle id `ai.storyteller.artcraft-launcher`,
/// `ArtCraft Launcher.app`). Not part of [`SUITE`].
pub const LAUNCHER: App = App::new("artcraft-launcher", "ArtCraft Launcher", "Installs and updates the Craft App Suite", "storytold/craft-launcher");

/// The Craft App Suite, in getartcraft.com's display order.
pub const SUITE: [App; 15] = [
    PHOTOCRAFT,
    VECTORCRAFT,
    FILMCRAFT,
    LIGHTCRAFT,
    PDFCRAFT,
    EFFECTCRAFT,
    DESIGNCRAFT,
    FIGCRAFT,
    LOCALPLAYER,
    SOUNDCRAFT,
    DECKCRAFT,
    LOCALSLIDES,
    LOCALSHEETS,
    CADCRAFT,
    GRIDCRAFT,
];

/// The suite's app with this id.
pub fn by_id(id: &str) -> Option<&'static App> {
    SUITE.iter().find(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifteen_apps_with_unique_ids_in_their_own_repos() {
        let mut ids: Vec<_> = SUITE.iter().map(|a| a.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SUITE.len());
        for a in &SUITE {
            if a.repo.starts_with("storytold/") {
                assert_eq!(a.repo, format!("storytold/{}", a.id));
                assert_eq!(a.name.to_lowercase(), a.id);
            }
        }
        assert!(by_id(LAUNCHER.id).is_none());
        assert_eq!(by_id("pdfcraft").map(|a| a.former_ids), Some(&["printcraft"][..]));
        assert_eq!(by_id("localslides").map(|a| (a.name, a.publisher)), Some(("Local Slides", "Local Slides contributors")));
        assert_eq!(by_id("localsheets").map(|a| (a.name, a.publisher, a.repo)), Some(("Local Sheets", "Anon5T4R", "Anon5T4R/local-sheets")));
    }
}
