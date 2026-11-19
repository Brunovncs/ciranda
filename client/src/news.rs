//! What changed in each version, for the "What's new" dialog (`ui::news`) that greets an update.
//! The notes ship inside the program, in both languages, so they show offline and always match
//! the version running. Whoever skipped versions gets every one they missed, newest first.
//!
//! A release adds its entry at the top of `NEWS`, with the version in `Cargo.toml`: a test fails
//! until it does, and the release workflow runs the tests. Plain sentences for people using
//! Ciranda, not commit messages.

use crate::core::update::is_newer;

pub const NEWS: &[Entry] = &[
    Entry {
        version: "4.7.6",
        date: (2026, 11, 19),
        notes: &[
            new(
                "After an update, this window shows what changed, every version you skipped included.",
                "Depois de cada atualização, esta janela mostra o que mudou, incluindo as versões que você pulou.",
            ),
            new(
                "The whole history is in Settings, under Advanced: \"What's new\", next to the update check.",
                "O histórico completo fica em Configurações, em Avançado: \"Novidades\", ao lado da busca por atualizações.",
            ),
        ],
    },
    Entry {
        version: "4.7.5",
        date: (2026, 11, 16),
        notes: &[
            new(
                "Select the text of messages with the mouse, in channels and private conversations, and copy it with Ctrl+C or a right click.",
                "Agora dá para selecionar o texto das mensagens com o mouse, nos canais e nas conversas privadas, e copiar com Ctrl+C ou com o botão direito.",
            ),
            better(
                "In a call, people without a camera stay on the stage with their picture, and a red badge shows who is muted or deafened.",
                "Na chamada, quem está sem câmera continua no palco com a foto, e um selo vermelho mostra quem está mutado ou com o som desligado.",
            ),
            better(
                "Cameras show the whole picture: widescreen is preferred, so nothing is cut off at the top and bottom.",
                "A câmera mostra a imagem inteira: o formato widescreen tem preferência, então nada é cortado em cima e embaixo.",
            ),
            better(
                "Messages, members, sliders and the cards in a call can be reached with Tab.",
                "Mensagens, membros, controles deslizantes e os cards da chamada agora são alcançados com Tab.",
            ),
        ],
    },
    Entry {
        version: "4.7.4",
        date: (2026, 11, 16),
        notes: &[
            better(
                "Tab and Shift+Tab move through buttons, menus and channel lists, and Enter or Space presses what has focus.",
                "Tab e Shift+Tab percorrem botões, menus e listas de canais, e Enter ou Espaço aciona o que estiver em foco.",
            ),
            better(
                "Secondary text and links are easier to read in every theme.",
                "Textos secundários e links ficaram mais fáceis de ler em todos os temas.",
            ),
            better(
                "Switching channels and servers is quicker, and profile pictures load faster.",
                "Trocar de canal e de servidor ficou mais rápido, e as fotos de perfil carregam mais depressa.",
            ),
            fixed(
                "Joining a voice channel right as Ciranda connects no longer gets stuck on \"Reconnecting\".",
                "Entrar num canal de voz logo que o Ciranda conecta não fica mais preso em \"Reconectando\".",
            ),
            fixed(
                "\"Accept and send\" no longer sends text typed after the message that failed.",
                "\"Aceitar e enviar\" não manda mais o texto digitado depois da mensagem que falhou.",
            ),
        ],
    },
    Entry {
        version: "4.7.3",
        date: (2026, 11, 9),
        notes: &[
            new(
                "@todos, @all and @here call everyone in the channel, as @everyone does.",
                "@todos, @all e @here chamam todo mundo do canal, como o @everyone.",
            ),
            new(
                "Mentions reach Ciranda for Android even with the app closed, on servers updated for it.",
                "Menções chegam no Ciranda para Android mesmo com o app fechado, nos servidores já atualizados.",
            ),
        ],
    },
    Entry {
        version: "4.7.2",
        date: (2026, 11, 9),
        notes: &[new(
            "Invite links: right-click a server on the bar and choose \"Copy invite link\". Pasting the link as the server's address fills in the address and the password.",
            "Links de convite: clique com o botão direito num servidor da barra e escolha \"Copiar convite\". Colar o link no endereço do servidor já preenche o endereço e a senha.",
        )],
    },
    Entry {
        version: "4.7.1",
        date: (2026, 11, 6),
        notes: &[fixed(
            "With three or more cameras on, the grid cut the last ones off. Four cameras now sit two by two.",
            "Com três ou mais câmeras ligadas, a grade cortava as últimas. Agora quatro câmeras ficam duas por fileira.",
        )],
    },
    Entry {
        version: "4.7.0",
        date: (2026, 11, 3),
        notes: &[
            new(
                "Cirandinha: music for everyone in a voice channel. Search, or paste a YouTube or Spotify link or playlist, and it plays for everyone at the same moment.",
                "Cirandinha: música para todo mundo no canal de voz. Pesquise ou cole um link ou playlist do YouTube ou do Spotify, e ela toca para todos ao mesmo tempo.",
            ),
            new(
                "The queue is shared: anyone in the channel can play a song next, move, remove, shuffle or repeat.",
                "A fila é compartilhada: qualquer pessoa no canal pode tocar uma música em seguida, mover, remover, embaralhar ou repetir.",
            ),
            better(
                "Every song plays at the same loudness, and the music's volume is yours alone.",
                "Todas as músicas tocam no mesmo volume, e o volume da música é só seu.",
            ),
        ],
    },
];

/// The last version from before this dialog. Someone updating from it, or from older, has no
/// record of what they saw, and gets what came after it.
pub const BEFORE_NEWS: &str = "4.7.5";

pub struct Entry {
    pub version: &'static str,
    /// Year, month, day.
    pub date: (i32, u32, u32),
    pub notes: &'static [Note],
}

pub struct Note {
    pub kind: Kind,
    en: &'static str,
    pt: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    New,
    Better,
    Fixed,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::New, Kind::Better, Kind::Fixed];
}

const fn new(en: &'static str, pt: &'static str) -> Note {
    Note { kind: Kind::New, en, pt }
}

const fn better(en: &'static str, pt: &'static str) -> Note {
    Note { kind: Kind::Better, en, pt }
}

const fn fixed(en: &'static str, pt: &'static str) -> Note {
    Note { kind: Kind::Fixed, en, pt }
}

impl Note {
    pub fn text(&self) -> &'static str {
        tr!(self.en, self.pt)
    }
}

impl Entry {
    pub fn date(&self) -> Option<chrono::NaiveDate> {
        let (y, m, d) = self.date;
        chrono::NaiveDate::from_ymd_opt(y, m, d)
    }
}

/// The version running.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The entries after `seen` up to `current`, newest first.
pub fn since(seen: &str, current: &str) -> Vec<&'static Entry> {
    NEWS.iter().filter(|e| is_newer(e.version, seen) && !is_newer(e.version, current)).collect()
}

/// Every entry up to `current`, newest first: the whole history, from Settings.
pub fn up_to(current: &str) -> Vec<&'static Entry> {
    NEWS.iter().filter(|e| !is_newer(e.version, current)).collect()
}

/// The version whose news was last shown, from the setting: none for a new install, which has
/// nothing to catch up on, and `BEFORE_NEWS` for one from before the setting.
pub fn seen(setting: &str, has_servers: bool) -> Option<&str> {
    match setting.trim() {
        "" if has_servers => Some(BEFORE_NEWS),
        "" => None,
        v => Some(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::update::is_newer;

    #[test]
    fn this_version_has_its_news() {
        assert_eq!(NEWS.first().map(|e| e.version), Some(current()), "add an entry for {} at the top of NEWS in src/news.rs", current());
    }

    #[test]
    fn entries_go_newest_first_and_read_well() {
        for pair in NEWS.windows(2) {
            assert!(is_newer(pair[0].version, pair[1].version), "{} must come before {}", pair[0].version, pair[1].version);
            assert!(pair[0].date >= pair[1].date, "{} is dated before {}", pair[0].version, pair[1].version);
        }
        for e in NEWS {
            assert!(e.date().is_some(), "{} has no such date", e.version);
            assert!(!e.notes.is_empty(), "{} says nothing", e.version);
            for n in e.notes {
                for text in [n.en, n.pt] {
                    assert!(!text.trim().is_empty(), "{} has an empty note", e.version);
                    assert!(!text.contains('—'), "{}: no em dashes in the notes", e.version);
                }
            }
        }
    }

    #[test]
    fn skipped_versions_all_show_newest_first() {
        let versions = |v: Vec<&Entry>| v.iter().map(|e| e.version).collect::<Vec<_>>();
        assert_eq!(versions(since("4.7.2", "4.7.5")), ["4.7.5", "4.7.4", "4.7.3"]);
        assert_eq!(versions(since("4.7.4", "4.7.5")), ["4.7.5"]);
        assert!(since("4.7.5", "4.7.5").is_empty());
        // A version put back after an update shows nothing it already did, nor what is ahead.
        assert!(since("4.7.5", "4.7.3").is_empty());
        assert_eq!(versions(since("4.6.0", "4.7.1")), ["4.7.1", "4.7.0"]);
        assert_eq!(versions(up_to("4.7.2")), ["4.7.2", "4.7.1", "4.7.0"]);
    }

    #[test]
    fn where_the_news_starts() {
        assert_eq!(seen("", false), None);
        assert_eq!(seen("", true), Some(BEFORE_NEWS));
        assert_eq!(seen("4.8.0", true), Some("4.8.0"));
        assert_eq!(seen("4.8.0", false), Some("4.8.0"));
    }
}
