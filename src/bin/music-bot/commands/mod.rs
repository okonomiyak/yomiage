pub mod dashboard;
mod help;
mod music;
mod playlist;
mod settings;
mod voice;

use crate::{Data, Error};

/// フレームワークに登録するコマンド一覧（音楽 Bot）。
pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![
        voice::join(),
        voice::leave(),
        music::play(),
        music::queue(),
        music::next(),
        music::stop(),
        music::volume(),
        playlist::playlist(),
        dashboard::dashboard(),
        settings::feature(),
        help::help(),
    ]
}
