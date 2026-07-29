mod about;
pub mod dashboard;
mod dict;
mod help;
pub mod music;
mod playlist;
pub mod settings;
mod stats;
mod timesignal;
mod voice;

use crate::{Data, Error};

/// フレームワークに登録するコマンド一覧。
pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![
        voice::join(),
        voice::leave(),
        voice::skip(),
        voice::bind(),
        voice::unbind(),
        settings::voice(),
        settings::speed(),
        settings::pitch(),
        settings::intonation(),
        settings::maxlength(),
        settings::feature(),
        settings::config(),
        dict::dict(),
        about::about(),
        help::help(),
        music::play(),
        music::queue(),
        music::next(),
        music::stop(),
        music::volume(),
        playlist::playlist(),
        dashboard::dashboard(),
        timesignal::timesignal(),
        stats::stats(),
    ]
}
