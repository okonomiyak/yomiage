mod about;
mod dict;
mod help;
pub mod settings;
mod stats;
mod timesignal;
mod voice;

use crate::{Data, Error};

/// フレームワークに登録するコマンド一覧（読み上げ Bot）。
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
        timesignal::timesignal(),
        stats::stats(),
    ]
}
