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
        version: "4.9.0",
        date: (2026, 11, 19),
        notes: &[
            new(
                "Colour themes, in Settings under Appearance: Vice City, just as Discord has it, and Middle-earth, Bullworth, Division, Blocks, Night City and Outer Wilds, each with a picture of its own behind the window.",
                "Temas coloridos, em Configurações, em Aparência: o Vice City, igual ao do Discord, e Terra Média, Bullworth, Divisão, Blocos, Night City e Outer Wilds, cada um com uma cena própria atrás da janela.",
            ),
            new(
                "The themes move: rain and glitches in Night City, snow in Division, embers in Middle-earth, clouds in Blocks, a campfire and a supernova every 22 minutes in Outer Wilds. They stop while the window is in the background, and can be turned off.",
                "Os temas se mexem: chuva e glitches no Night City, neve na Divisão, brasas na Terra Média, nuvens nos Blocos, fogueira e uma supernova a cada 22 minutos no Outer Wilds. Param quando a janela fica em segundo plano, e dá para desligar.",
            ),
            new(
                "A name style for each theme, picked in your profile, in a typeface of its own: the Night City one glitches, the Blocks one has the game's shadow.",
                "Um estilo de nome para cada tema, escolhido no seu perfil, com uma fonte própria: o do Night City dá glitch, o dos Blocos tem a sombra do jogo.",
            ),
            new(
                "An app icon for each theme, on the window, the taskbar and the notification area.",
                "Um ícone do aplicativo para cada tema, na janela, na barra de tarefas e ao lado do relógio.",
            ),
            fixed(
                "Ciranda opens again on the screen it was on, at the same size, also when that screen has another scale.",
                "O Ciranda volta a abrir na tela em que estava, do mesmo tamanho, mesmo quando essa tela tem outra escala.",
            ),
            better("Name styles need the server updated to 4.9.0.", "Os estilos de nome precisam do servidor atualizado para a 4.9.0."),
        ],
    },
    Entry {
        version: "4.8.0",
        date: (2026, 11, 19),
        notes: &[
            new(
                "Playing now: the game you have open shows beside your name, as \"Playing VALORANT\". Only its name leaves your computer, and it can be turned off in Settings, under In game.",
                "Jogando agora: o jogo que você está jogando aparece ao lado do seu nome, como \"Jogando VALORANT\". Só o nome dele sai do seu computador, e dá para desligar em Configurações, em Durante o jogo.",
            ),
            new(
                "Animated names: rainbow, aurora, fire, ocean, gold or candy, picked in your profile, moving in the member list, in voice and on your card.",
                "Nomes animados: arco-íris, aurora, fogo, oceano, ouro ou doce, escolhidos no seu perfil, em movimento na lista de membros, na voz e no seu cartão.",
            ),
            new(
                "The year in review: in December, a few slides of what your server did together, from the chat to the hours in voice, the games and the songs, and yours too.",
                "A Retrospectiva: em dezembro, alguns slides do que o seu servidor fez junto, do chat às horas na voz, os jogos e as músicas, e a sua também.",
            ),
            new(
                "See who is typing, in channels and private conversations.",
                "Veja quem está digitando, nos canais e nas conversas privadas.",
            ),
            better(
                "Smoother everywhere: opening and closing things, switching channels and servers, watching a stream full screen and muting all move softly, and corners are rounder.",
                "Mais suave em tudo: abrir e fechar, trocar de canal e de servidor, assistir uma live em tela cheia e mutar agora têm animação, e os cantos ficaram mais arredondados.",
            ),
            better(
                "The muted badge is a quiet glass instead of a red disc, and new messages rise into place.",
                "O selo de mutado virou um vidro discreto em vez do disco vermelho, e as mensagens novas entram com animação.",
            ),
            better(
                "Ctrl and +, - or 0 change the size of the interface, and a paste too long for a message is sent as a text file.",
                "Ctrl com +, - ou 0 muda o tamanho da interface, e um texto colado longo demais para uma mensagem vai como arquivo de texto.",
            ),
            better(
                "Playing now, animated names, typing and the year in review need the server updated to 4.8.0.",
                "Jogando agora, nomes animados, digitando e a Retrospectiva precisam do servidor atualizado para a 4.8.0.",
            ),
        ],
    },
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
