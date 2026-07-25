pub mod settings;
mod voice;

use crate::{Data, Error};

/// フレームワークに登録するコマンド一覧。
pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![
        voice::join(),
        voice::leave(),
        settings::voice(),
        settings::speed(),
        settings::pitch(),
        settings::intonation(),
        settings::config(),
    ]
}
