//! The Commands panel: the right-hand column of the original ISE, listing every command the
//! session can see and building a command line out of the ones chosen from it.
//!
//! The engine answers a single verb, `M`, and it is the authority on what the answer means
//! (see `PowerShellEngineHost/CommandMetadata.cs`). This file only reads it, but it reads it
//! in one place, because both halves have to agree on the wire format exactly:
//!
//! ```text
//! M <kind> <part> <of> <reply> <payload>
//! ```
//!
//! `kind` is `i` for the command index, `d` for one command's parameters, `e` for a failure.
//! The payload is too big for one record, so it is cut into `of` chunks numbered from 1: the
//! whole answer is only meaningful once all of them have arrived, which is [`MetadataStream`].
//! Inside the payload the separators are three characters that cannot appear in a command
//! name - `U+001E` between rows, `U+001F` between row fields, `U+001D` between a parameter's
//! fields - and the engine strips them out of the values it writes, so a field count is
//! something this file may rely on.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// Separates one row of a payload from the next.
const ROW: char = '\u{1e}';
/// Separates the fields of a row.
const FIELD: char = '\u{1f}';
/// Separates the fields of a parameter row, which is itself one field of the command row.
const PART: char = '\u{1d}';

/// How much reassembled payload to accept before deciding the answer has gone wrong. The
/// command index on a machine with the Azure modules installed is around a quarter of a
/// megabyte, so this only ever catches a stream that cannot be believed - and without it a
/// confused engine could grow the panel's memory without limit.
const MAX_PAYLOAD: usize = 32 * 1024 * 1024;

/// What an answer is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MetadataKind {
    /// The list of every command the session can see.
    Index,
    /// One command: its syntax and its parameters.
    Detail,
    /// A refusal or a failure, with the engine's own words about it. This is the default
    /// because a stream that has not been told otherwise is not holding anything good.
    #[default]
    Error,
}

/// One complete answer, with every chunk put back in order.
pub struct MetadataReply {
    pub kind: MetadataKind,
    pub payload: String,
}

/// Puts the engine's chunked `M` records back together.
///
/// The engine answers one request at a time and writes its chunks in order, so the answers
/// arrive in order too. What does *not* arrive in order is everything else: a command that
/// finishes while a list is being read puts its output between two chunks, and the panel may
/// ask for a second answer before the first has been picked up. Hence the reply number, which
/// the engine counts up with every answer it gives - including abandoned ones.
#[derive(Default)]
pub struct MetadataStream {
    /// The reply number being assembled, or 0 when nothing is in hand. The engine's counter
    /// starts at 1, so 0 is not a number an answer can carry.
    reply: i64,
    kind: MetadataKind,
    parts: String,
    /// Chunks seen, and chunks the header says are coming.
    got: i64,
    want: i64,
}

impl MetadataStream {
    /// Takes one `M` record and returns the answer it completed, if it completed one.
    ///
    /// A record whose number does not follow on is not stitched in: it means a chunk was
    /// lost, and an answer with a hole in it would show the user a command list that looks
    /// complete. That is worse than the stale list it replaces, so the partial answer is
    /// reported as a failure and the panel keeps what it had.
    pub fn feed(
        &mut self,
        kind: &str,
        part: i64,
        of: i64,
        reply: i64,
        payload: &str,
    ) -> Option<MetadataReply> {
        let kind = match kind {
            "i" => MetadataKind::Index,
            "d" => MetadataKind::Detail,
            _ => MetadataKind::Error,
        };
        let of = of.max(1);

        if self.reply != reply || part == 1 {
            // Something new: whatever was half-assembled belongs to an answer nobody is
            // waiting for any more, and the panel has moved on.
            self.reply = reply;
            self.kind = kind;
            self.parts.clear();
            self.got = 0;
            self.want = of;
        }

        if part != self.got + 1 {
            self.reset();
            return Some(MetadataReply {
                kind: MetadataKind::Error,
                payload: "The engine's reply arrived out of order, so it was dropped. Try again."
                    .to_string(),
            });
        }

        self.parts.push_str(payload);
        self.got = part;

        if self.parts.len() > MAX_PAYLOAD {
            self.reset();
            return Some(MetadataReply {
                kind: MetadataKind::Error,
                payload: "The engine's reply was too large to read. Try again.".to_string(),
            });
        }

        if self.got < self.want {
            return None;
        }

        let kind = self.kind;
        let payload = std::mem::take(&mut self.parts);
        self.reply = 0;
        self.got = 0;
        self.want = 1;
        Some(MetadataReply { kind, payload })
    }

    /// Drops whatever is in hand, for example because the engine is going away.
    pub fn reset(&mut self) {
        self.reply = 0;
        self.kind = MetadataKind::Error;
        self.parts.clear();
        self.got = 0;
        self.want = 1;
    }
}

/// One row of the command index.
pub struct CommandEntry {
    pub name: String,
    /// The module that provided it, empty for the commands built into PowerShell itself and
    /// for a handful of stubs the engine cannot attribute.
    pub module: String,
}

/// One command, as the `d` answer describes it.
pub struct CommandDetail {
    /// The command the reply is about. For an alias this is the name the user picked, while
    /// `syntax` describes the command behind it.
    pub name: String,
    pub module: String,
    /// Empty when it would only repeat the syntax line, which is what the engine does for a
    /// command with no help file installed - most of them.
    pub synopsis: String,
    /// The parameter sets, as PowerShell writes them.
    pub syntax: String,
    pub parameters: Vec<ParameterInfo>,
}

/// One parameter of a command.
pub struct ParameterInfo {
    pub name: String,
    /// The short .NET type name, which is what the original ISE shows in its Type column.
    pub type_name: String,
    /// True when any parameter set requires it. A parameter may sit in several sets and the
    /// engine folds them together, which is what the syntax line shows.
    pub mandatory: bool,
    /// The lowest position any set gives it, or `None` when it is never positional.
    pub position: Option<i32>,
    pub is_switch: bool,
    /// True for the parameters every cmdlet has, which the panel groups apart.
    pub is_common: bool,
    /// Only filled in when a help file is installed for the module.
    pub description: String,
}

/// Reads the index payload: `name U+001F module U+001F parameter names`, one row each.
///
/// The third field is deliberately not kept. It is empty for almost every command - a module
/// that is only stubbed into the session has no parameter attributes to read until it is
/// imported - and where it is not empty it is no better than a hint, while the detail answer
/// is authoritative. Keeping it would mean two sources of truth in the panel for one fact.
pub fn parse_index(payload: &str) -> Vec<CommandEntry> {
    let mut entries = Vec::new();
    for row in payload.split(ROW) {
        let fields: Vec<&str> = row.split(FIELD).collect();
        let [name, module, _] = fields.as_slice() else { continue };
        if name.is_empty() {
            continue;
        }
        entries.push(CommandEntry {
            name: (*name).to_string(),
            module: (*module).to_string(),
        });
    }
    // The engine lists commands the way `Get-Command` yields them, which is grouped by
    // module and by provider. The original ISE shows one alphabetical list, and that is what
    // a user scanning for `Get-Process` expects to have to look through.
    entries.sort_by(|a, b| name_cmp(&a.name, &b.name));
    entries
}

/// Reads a detail payload: one `C` row about the command, then one `P` row per parameter.
pub fn parse_detail(payload: &str) -> CommandDetail {
    let mut detail = CommandDetail {
        name: String::new(),
        module: String::new(),
        synopsis: String::new(),
        syntax: String::new(),
        parameters: Vec::new(),
    };

    for row in payload.split(ROW) {
        if let Some(rest) = row.strip_prefix('C') {
            let fields: Vec<&str> = rest.split(FIELD).collect();
            let [_, name, module, synopsis, syntax] = fields.as_slice() else {
                continue;
            };
            detail.name = (*name).to_string();
            detail.module = (*module).to_string();
            detail.synopsis = (*synopsis).to_string();
            detail.syntax = (*syntax).to_string();
        } else if let Some(rest) = row.strip_prefix('P') {
            let fields: Vec<&str> = rest.split(PART).collect();
            let [_, name, type_name, mandatory, position, is_switch, is_common, description] =
                fields.as_slice()
            else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            detail.parameters.push(ParameterInfo {
                name: (*name).to_string(),
                type_name: (*type_name).to_string(),
                mandatory: *mandatory == "1",
                position: position.parse::<i32>().ok().filter(|p| *p >= 0),
                is_switch: *is_switch == "1",
                is_common: *is_common == "1",
                description: (*description).to_string(),
            });
        }
    }

    // The command's own parameters at the top - the ones it cannot be run without first, then by
    // position, then by name - and the ones every cmdlet has at the bottom. They are kept in one
    // block because the parameter list draws a divider where they start, and within the block
    // they sort by name so a tick that rebuilds the list cannot shuffle the rows about.
    detail.parameters.sort_by(|a, b| {
        a.is_common
            .cmp(&b.is_common)
            .then_with(|| b.mandatory.cmp(&a.mandatory))
            .then_with(|| match (a.position, b.position) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            })
            .then_with(|| name_cmp(&a.name, &b.name))
    });
    detail
}

// ------------------------------------------------------------------ what the panel knows

/// How many times the panel will read the command list on its own before it stops trying and
/// leaves the explanation up for the user to act on. The engine's own cache makes a repeat
/// read cheap, but a read while the shell is busy is refused, and refusing in a loop is not
/// a plan. `Refresh Command List` starts the count again.
const MAX_READS: u32 = 3;

/// Ticks to wait before trying again after the engine refused a read: one second of the pump's
/// 40 ms timer. A refusal is the shell saying "not yet", and asking forty times a second is not
/// how a busy shell becomes free.
const READ_BACKOFF: u32 = 25;

/// Wording the engine uses when it refused because a command is running
/// (`BusyMessage` in `Program.cs`). It is worth telling apart from a failure to read: the
/// first is worth waiting for, the second is worth stopping for.
const BUSY_WORDS: &str = "running a command";

/// What the panel knows about the session's commands, and what it is waiting to hear.
#[derive(Default)]
pub struct CommandStore {
    /// Every command from the last index answer, in name order.
    entries: Vec<CommandEntry>,
    /// The modules those commands came from, in name order, with the empty name - the
    /// commands no module owns - last. A machine with the Azure and Winget modules installed
    /// has around ninety of these, and 36 of its commands have no module at all.
    modules: Vec<String>,
    /// Details already read, keyed by the name the answer itself reported.
    details: HashMap<String, CommandDetail>,
    /// True once an index answer has been read. An empty list and an unread one are not the
    /// same answer, and the panel says different things about each.
    listed: bool,
    /// Reads started and not answered, so one request does not become twenty.
    reads: u32,
    /// Ticks left before another read of the list may be sent, counted down by [`tick`].
    backoff: u32,
    /// The command list has been asked for and not answered.
    pending_index: bool,
    /// The next read of the list is one the user asked for, so the engine is told to look at
    /// the session again instead of answering from the cache it keeps of the last one.
    hard: bool,
    /// One command's parameters have been asked for and not answered. Asked-for and answered
    /// reads are tracked apart because the panel can select a command while the list itself is
    /// still on its way, and the engine answers in the order it is asked.
    pending_detail: Option<String>,
    /// Attempts at the command named by `detail_read_for`. A command the session has stopped
    /// knowing about - a module removed since the list was read - is answered with an error
    /// every single time, and asking again the moment that arrives is twenty-five reads a second
    /// against a shell that can only keep saying no, with the completion requests queued behind
    /// them. So a description gets the same three attempts the list gets, and no more.
    detail_reads: u32,
    detail_read_for: Option<String>,
    /// The last failure to read, in the engine's own words, kept until the next attempt.
    error: Option<String>,
}

impl CommandStore {
    /// Takes a finished answer. Gives back true when the panel has to be redrawn.
    pub fn apply(&mut self, reply: MetadataReply) -> bool {
        match reply.kind {
            MetadataKind::Index => {
                self.pending_index = false;
                self.error = None;
                self.entries = parse_index(&reply.payload);
                self.modules = module_names(&self.entries);
                self.listed = true;
                true
            }
            MetadataKind::Detail => {
                // The name comes from the answer rather than from the row the user had picked
                // when the request went out: by the time it arrives they may well have moved
                // on, and an answer about another command should then warm the cache instead
                // of painting the wrong syntax behind the name they are now looking at.
                let detail = parse_detail(&reply.payload);
                let name = detail.name.clone();
                self.pending_detail = None;
                self.error = None;
                self.details.insert(name, detail);
                true
            }
            MetadataKind::Error => {
                let was_index = self.pending_index;
                // Which read this answer is about decides what a refusal costs and what the
                // explanation has to call it, so it is taken out of the store before anything
                // else is cleared.
                let asked = self.pending_detail.take();
                let busy = reply.payload.contains(BUSY_WORDS);
                self.pending_index = false;
                self.error = Some(if reply.payload.trim().is_empty() {
                    if asked.is_some() {
                        "The engine could not read this command.".to_string()
                    } else {
                        "The engine could not read the command list.".to_string()
                    }
                } else {
                    reply.payload
                });
                // A failed read is not a session with no commands, so the panel has to be
                // willing to ask again - but only `MAX_READS` times, and then it leaves the
                // engine's explanation showing rather than spinning.
                if was_index {
                    self.listed = false;
                }
                if busy {
                    // The engine said "not yet" rather than "cannot": the shell was still
                    // holding the runspace, which the front end cannot see from here, because
                    // a startup script or another read holds it just as well as a command
                    // does. That is worth waiting for, so it is not charged against the
                    // attempts - but it waits a second rather than the pump's next tick.
                    if was_index {
                        self.reads = self.reads.saturating_sub(1);
                    }
                    if asked.is_some() {
                        self.detail_reads = self.detail_reads.saturating_sub(1);
                    }
                    self.backoff = READ_BACKOFF;
                }
                true
            }
        }
    }

    /// Counts one pump tick. Kept in the store rather than in the panel so the wait outlives
    /// whatever else happens to the column in the meantime.
    pub fn tick(&mut self) {
        self.backoff = self.backoff.saturating_sub(1);
    }

    /// True when the command list should be asked for: it has never been read, nothing is on
    /// its way, the panel has not already given up on it, and it is not sitting out the second
    /// after a refusal.
    pub fn index_wanted(&self) -> bool {
        !self.listed
            && !self.pending_index
            && self.reads < MAX_READS
            && self.backoff == 0
    }

    /// Records that the list has been asked for.
    pub fn note_index_requested(&mut self) {
        self.reads += 1;
        self.error = None;
        self.pending_index = true;
        self.hard = false;
    }

    /// True when the read about to be sent was asked for by name, in which case the engine has
    /// to be told to enumerate the session again rather than hand back what it remembered: a
    /// module imported since the last read has commands the cache has never seen.
    pub fn hard_refresh(&self) -> bool {
        self.hard
    }

    /// Forgets the list, so the next idle moment reads it again from the session rather than
    /// from the engine's cache. What is on show stays there until the new answer arrives.
    pub fn ask_refresh(&mut self) {
        self.listed = false;
        self.reads = 0;
        self.pending_index = false;
        self.pending_detail = None;
        self.error = None;
        self.hard = true;
        // A command the panel had stopped asking about gets its chances back: this is the user
        // saying the session has changed, which is exactly the reason the attempts failed.
        self.detail_reads = 0;
        self.detail_read_for = None;
        // The user asked with their own hand: the wait out for a refusal is not owed to the
        // engine then.
        self.backoff = 0;
    }

    /// True while the second after a refusal is being sat out.
    pub fn waiting(&self) -> bool {
        self.backoff > 0
    }

    /// True when a command's parameters are wanted: not in hand, not already on their way, not
    /// sat out after a refusal, and not something this many attempts have already failed at.
    pub fn detail_wanted(&self, name: &str) -> bool {
        !self.details.contains_key(name)
            && self.pending_detail.as_deref() != Some(name)
            && self.detail_reads_for(name) < MAX_READS
    }

    /// Records that one command's parameters have been asked for.
    pub fn note_detail_requested(&mut self, name: &str) {
        self.error = None;
        if self.detail_read_for.as_deref() != Some(name) {
            // A different command is a fresh set of attempts: one that cannot be described
            // should not use up the chances of the next one the user clicks on.
            self.detail_read_for = Some(name.to_string());
            self.detail_reads = 0;
        }
        self.detail_reads += 1;
        self.pending_detail = Some(name.to_string());
    }

    /// True when this command's parameters are not coming: not in hand, and no attempt left to
    /// bring them. `detail_wanted` cannot answer that - it is also false while a read is on its
    /// way - and the syntax area should not say "Reading..." about a read it has given up on.
    pub fn detail_abandoned(&self, name: &str) -> bool {
        !self.details.contains_key(name)
            && self.pending_detail.as_deref() != Some(name)
            && self.detail_reads_for(name) >= MAX_READS
    }

    /// Attempts made at describing this command, which is nothing for a command other than the
    /// one the last request was about.
    fn detail_reads_for(&self, name: &str) -> u32 {
        if self.detail_read_for.as_deref() == Some(name) {
            self.detail_reads
        } else {
            0
        }
    }

    pub fn entries(&self) -> &[CommandEntry] {
        &self.entries
    }

    pub fn modules(&self) -> &[String] {
        &self.modules
    }

    pub fn is_listed(&self) -> bool {
        self.listed
    }

    pub fn detail(&self, name: &str) -> Option<&CommandDetail> {
        self.details.get(name)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The names the filters leave, as indexes into `entries`. Both filters are what the user
    /// has typed, so this runs on every keystroke over four thousand rows: a substring test
    /// and a module compare is cheap enough not to need a better answer.
    pub fn visible(&self, module: Option<&str>, find: &str) -> Vec<usize> {
        let find = find.trim().to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| match module {
                Some(name) => entry.module == name,
                None => true,
            })
            .filter(|(_, entry)| {
                find.is_empty() || entry.name.to_lowercase().contains(&find)
            })
            .map(|(index, _)| index)
            .collect()
    }
}

/// The distinct modules of an index, in name order, with the commands no module owns last:
/// those are worth being able to look at on their own, and putting them first would make the
/// dropdown start with an entry that names nothing.
fn module_names(entries: &[CommandEntry]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names: Vec<String> = entries
        .iter()
        .map(|entry| entry.module.clone())
        .filter(|name| seen.insert(name.clone()))
        .collect();
    names.sort_by(|a, b| match (a.is_empty(), b.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => name_cmp(a, b),
    });
    names
}

/// Compares two names the way PowerShell does, which is without regard to case.
fn name_cmp(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assembles a payload out of chunks exactly as the engine would write them, and feeds
    /// them to the stream. Returns the answer the last one completes.
    fn stream(parts: &[&str], reply: i64) -> Option<MetadataReply> {
        let mut s = MetadataStream::default();
        let of = parts.len() as i64;
        let mut done = None;
        for (i, part) in parts.iter().enumerate() {
            done = s.feed("i", i as i64 + 1, of, reply, part);
        }
        done
    }

    #[test]
    fn index_rows_are_read_back() {
        let payload = "Get-ChildItem\u{1f}Microsoft.PowerShell.Management\u{1f}Path,Recurse\u{1e}\
                       cd\u{1f}\u{1f}\u{1e}";
        let entries = parse_index(payload);
        assert_eq!(entries.len(), 2);
        // Sorted, so the alias comes first however the engine ordered the reply.
        assert_eq!(entries[0].name, "cd");
        assert_eq!(entries[0].module, "");
        assert_eq!(entries[1].name, "Get-ChildItem");
        assert_eq!(entries[1].module, "Microsoft.PowerShell.Management");
    }

    #[test]
    fn a_malformed_index_row_is_skipped_not_guessed() {
        let entries = parse_index("Get-Date\u{1f}Module\u{1e}broken-row\u{1e}A\u{1f}B\u{1f}x\u{1e}");
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["A"]);
    }

    /// The two filters, on their own and together. An empty Find box means "everything", not
    /// "nothing", and the commands no module owns are a group of their own: both are cases
    /// where the list ends up with rows that a naive comparison would drop or keep wrongly.
    #[test]
    fn the_two_filters_narrow_the_list_and_leave_alone_what_they_do_not_name() {
        let mut store = CommandStore::default();
        store.apply(reply(
            MetadataKind::Index,
            "Get-ChildItem\u{1f}Microsoft.PowerShell.Management\u{1f}\u{1e}\
             cd\u{1f}\u{1f}\u{1e}\
             Get-Process\u{1f}Microsoft.PowerShell.Management\u{1f}\u{1e}",
        ));
        // Sorted, so: 0 cd (no module), 1 Get-ChildItem, 2 Get-Process.
        assert_eq!(store.visible(None, ""), [0, 1, 2]);
        assert_eq!(store.visible(None, "   "), [0, 1, 2], "spaces are not a filter");
        assert_eq!(store.visible(None, "get-c"), [1], "Find ignores case");
        assert_eq!(
            store.visible(Some("Microsoft.PowerShell.Management"), ""),
            [1, 2],
            "a module names its own commands only"
        );
        assert_eq!(store.visible(Some(""), ""), [0], "the no-module group");
        assert_eq!(store.visible(Some("Microsoft.PowerShell.Management"), "child"), [1]);
        // A module with commands, none of which match: the honest answer is an empty list, and
        // the panel has to cope with a selection that no longer points at anything.
        assert!(store.visible(Some(""), "child").is_empty());
        assert!(store.visible(None, "nothing-called-this").is_empty());
    }

    #[test]
    fn chunks_are_put_back_in_order() {
        let done = stream(&["Get-Foo\u{1f}M\u{1f}\u{1e}", "Get-Bar\u{1f}M\u{1f}\u{1e}"], 7)
            .expect("the last chunk completes the answer");
        assert_eq!(done.kind, MetadataKind::Index);
        let entries = parse_index(&done.payload);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn an_earlier_answer_does_not_merge_into_a_later_one() {
        let mut s = MetadataStream::default();
        // The panel asked twice, so two answers are on the wire; the engine writes each
        // one's chunks together, and the first must not contaminate the second.
        assert!(s.feed("i", 1, 2, 3, "A\u{1f}M\u{1f}\u{1e}").is_none());
        let second = s.feed("i", 1, 1, 4, "B\u{1f}M\u{1f}\u{1e}");
        let done = second.expect("a one-chunk answer completes at once");
        let entries = parse_index(&done.payload);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "B");
        // And the abandoned answer cannot resume: its second part is now out of order.
        let stale = s.feed("i", 2, 2, 3, "C\u{1f}M\u{1f}\u{1e}").expect("reported, not stitched in");
        assert_eq!(stale.kind, MetadataKind::Error);
    }

    #[test]
    fn a_missing_chunk_is_reported_rather_than_stitched() {
        let mut s = MetadataStream::default();
        s.feed("i", 1, 3, 1, "A\u{1e}");
        let done = s.feed("i", 3, 3, 1, "C\u{1e}").expect("a gap is an answer of its own");
        assert_eq!(done.kind, MetadataKind::Error);
    }

    #[test]
    fn an_error_is_not_an_empty_command_list() {
        let mut s = MetadataStream::default();
        let done = s
            .feed("e", 1, 1, 2, "The session is running a command.")
            .expect("one chunk is a whole answer");
        assert_eq!(done.kind, MetadataKind::Error);
        assert!(done.payload.contains("running"));
    }

    #[test]
    fn detail_rows_are_read_back() {
        // The engine's rows arrive in its own order, so this payload puts the common parameter
        // first on purpose: the sort has to move it to the bottom.
        let payload = "C\u{1f}Get-ChildItem\u{1f}Microsoft.PowerShell.Management\u{1f}Gets the \
                       items\u{1f}Get-ChildItem [[-Path] <string[]>]\u{1e}\
                       P\u{1d}Verbose\u{1d}SwitchParameter\u{1d}0\u{1d}-1\u{1d}1\u{1d}1\u{1d}\u{1e}\
                       P\u{1d}Recurse\u{1d}SwitchParameter\u{1d}0\u{1d}-1\u{1d}1\u{1d}0\u{1d}\u{1e}\
                       P\u{1d}Path\u{1d}String[]\u{1d}1\u{1d}0\u{1d}0\u{1d}0\u{1d}\u{1e}";
        let detail = parse_detail(payload);
        assert_eq!(detail.name, "Get-ChildItem");
        assert_eq!(detail.synopsis, "Gets the items");
        assert_eq!(detail.syntax, "Get-ChildItem [[-Path] <string[]>]");
        assert_eq!(detail.parameters.len(), 3);

        // Mandatory first, then by position: Path, then the switches in name order.
        assert_eq!(detail.parameters[0].name, "Path");
        assert!(detail.parameters[0].mandatory);
        assert_eq!(detail.parameters[0].position, Some(0));
        assert!(!detail.parameters[0].is_switch);
        assert_eq!(detail.parameters[1].name, "Recurse");
        assert!(detail.parameters[1].is_switch);
        // A parameter that is never positional has no position to show.
        assert_eq!(detail.parameters[1].position, None);
        assert_eq!(detail.parameters[2].name, "Verbose");
        assert!(detail.parameters[2].is_common);
        assert_eq!(detail.parameters[2].description, "");
    }

    /// The parameter list puts one divider row where the common parameters start, which is only
    /// right if every one of them is below it: an interleaved list would put `-Verbose` above
    /// the line and `-ErrorAction` below it for no reason a user could see.
    #[test]
    fn the_common_parameters_are_kept_in_one_block_at_the_end() {
        let mut rows = String::from("C\u{1f}Get-Item\u{1f}Microsoft.PowerShell.Management\u{1f}\
                                     \u{1f}\u{1e}");
        for (name, common) in [
            ("Verbose", true),
            ("Path", false),
            ("ErrorAction", true),
            ("Filter", false),
            ("Confirm", true),
        ] {
            rows.push_str(&format!(
                "P\u{1d}{name}\u{1d}SwitchParameter\u{1d}0\u{1d}-1\u{1d}1\u{1d}{}\u{1d}\u{1e}",
                if common { 1 } else { 0 }
            ));
        }
        let detail = parse_detail(&rows);
        let names: Vec<&str> = detail
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["Filter", "Path", "Confirm", "ErrorAction", "Verbose"]
        );
        assert!(detail.parameters[..2].iter().all(|p| !p.is_common));
        assert!(detail.parameters[2..].iter().all(|p| p.is_common));
    }

    #[test]
    fn a_refusal_waits_a_second_and_is_not_charged_as_a_failure() {
        let mut store = CommandStore::default();
        store.note_index_requested();
        store.apply(reply(
            MetadataKind::Error,
            "The console is running a command. The command list is ready as soon as it finishes.",
        ));
        // Not now - and then again once the second is up, because a shell that is busy is not a
        // session whose commands cannot be read.
        assert!(store.waiting());
        assert!(!store.index_wanted());
        for _ in 0..READ_BACKOFF {
            store.tick();
        }
        assert!(!store.waiting());
        assert!(store.index_wanted());
    }

    #[test]
    fn a_failure_gives_up_after_three_attempts() {
        let mut store = CommandStore::default();
        for _ in 0..MAX_READS {
            assert!(store.index_wanted(), "attempt {}", store.reads);
            store.note_index_requested();
            store.apply(reply(
                MetadataKind::Error,
                "The engine read the session and found no commands to list.",
            ));
        }
        assert!(!store.index_wanted());
        // The explanation stays up for the user to act on, and Refresh starts the count again.
        assert!(store.error().unwrap().contains("no commands"));
        store.ask_refresh();
        assert!(store.index_wanted());
        assert!(store.hard_refresh());
    }

    /// A command the session has stopped knowing about - a module removed since the list was
    /// read, a function deleted - is refused every time it is asked about, and a refusal that is
    /// answered by another request straight away is a loop twenty-five deep every second, with
    /// the shell's own Tab completion queued behind it. Measured against the real engine before
    /// this bound existed: 156 reads about one dead name in six seconds.
    #[test]
    fn a_command_that_cannot_be_described_is_not_asked_about_for_ever() {
        let mut store = CommandStore::default();
        for attempt in 1..=MAX_READS {
            assert!(store.detail_wanted("gone"), "attempt {}", attempt);
            store.note_detail_requested("gone");
            store.apply(reply(
                MetadataKind::Error,
                "No command found with the name 'gone'.",
            ));
        }
        assert!(!store.detail_wanted("gone"));
        // The engine's own words stay up, which is what tells the user to refresh.
        assert!(store.error().unwrap().contains("No command found"));
        // The next command they click on is not punished for the last one's bad luck.
        assert!(store.detail_wanted("Get-Date"));
        store.note_detail_requested("Get-Date");
        store.apply(reply(
            MetadataKind::Detail,
            "C\u{1f}Get-Date\u{1f}Microsoft.PowerShell.Utility\u{1f}\u{1f}\u{1e}",
        ));
        assert!(!store.detail_wanted("Get-Date"));
        assert!(store.error().is_none());
    }

    /// A refusal because the shell is busy is "not yet", so it does not use up the attempts a
    /// command that cannot be described at all would need - the panel just waits a second.
    #[test]
    fn a_busy_refusal_about_one_command_costs_nothing_but_time() {
        let mut store = CommandStore::default();
        store.note_detail_requested("Get-Date");
        store.apply(reply(
            MetadataKind::Error,
            "The console is running a command. The command list is ready as soon as it finishes.",
        ));
        assert!(store.waiting());
        for _ in 0..READ_BACKOFF {
            store.tick();
        }
        assert!(!store.waiting());
        assert!(store.detail_wanted("Get-Date"));
        // Even three refusals in a row leave it willing to ask.
        for _ in 0..MAX_READS {
            store.note_detail_requested("Get-Date");
            store.apply(reply(
                MetadataKind::Error,
                "The console is running a command. The command list is ready as soon as it finishes.",
            ));
            for _ in 0..READ_BACKOFF {
                store.tick();
            }
        }
        assert!(store.detail_wanted("Get-Date"));
    }

    /// Builds a finished answer the way `MetadataStream` hands one over.
    fn reply(kind: MetadataKind, payload: &str) -> MetadataReply {
        MetadataReply {
            kind,
            payload: payload.to_string(),
        }
    }

    #[test]
    fn a_detail_reply_with_only_its_command_row_still_describes_the_command() {
        // An application, or a function with no parameters at all.
        let detail = parse_detail("C\u{1f}notepad\u{1f}\u{1f}\u{1f}notepad.exe\u{1e}");
        assert_eq!(detail.name, "notepad");
        assert!(detail.syntax == "notepad.exe");
        assert!(detail.parameters.is_empty());
    }
}
