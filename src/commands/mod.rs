mod about;
pub mod dashboard;
mod dict;
mod music;
pub mod settings;
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
        settings::config(),
        dict::dict(),
        about::about(),
        music::play(),
        music::queue(),
        music::next(),
        music::stop(),
        music::volume(),
        dashboard::dashboard(),
    ]
}
