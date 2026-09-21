//! Live probes for the questions `docs/api-v0.8.10.md` leaves open.
//!
//! These talk to the real API as you, so every one of them is `#[ignore]`d and
//! never runs in `cargo test`. Run one deliberately, read its verdict, and note
//! the answer in the spec notes:
//!
//! ```text
//! cargo test -p cs-api --test live_probes -- --ignored --nocapture \
//!     settings_notifications_merge_or_replace
//! ```
//!
//! **Why these are tests and not a script.** § Terms forbids bots — "automated
//! accounts that post, reply, follow, react, or otherwise act without a human
//! driving each action in real time". Each probe is one command a person types,
//! makes a handful of requests, and restores whatever it touched. They go
//! through [`Client`], so the rate limiter applies and nothing here can outrun
//! the budget the spec documents.
//!
//! **What is deliberately absent.** There is no probe for whether the grouped
//! rows in § Read Actions ("List guilds / members / a user's guilds | 30") are
//! one shared budget or one each. Answering that empirically means 60-90 reads
//! inside a minute against endpoints that exist to serve a human reading a
//! screen, which is the automated traffic § Terms prohibits, and it would put
//! the account at risk to settle a question one message to the API author
//! settles for free. The client instead logs any rate-limit headers a `429`
//! carries (`client.rs::log_rate_limit_headers`), so a limit hit in ordinary use
//! becomes the evidence.
//!
//! The session is read from the file cs-tui already saved; no credentials go on
//! the command line. Override with `CS_TUI_SESSION=/path/to/session.json`.
use std::path::PathBuf;

use cs_api::{Client, NewProgram, ProgramQuery, Runtime, SettingsUpdate, Tokens};

/// Build a client from the session cs-tui saved at its last login.
///
/// Panics with an explanation rather than returning an error: a probe with no
/// session has nothing to say, and the fix is to log in with the app.
async fn signed_in() -> Client {
    let path = session_path();
    let raw = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "no session at {}: {e}\nLog in with cs-tui first.",
            path.display()
        )
    });
    let tokens: Tokens =
        serde_json::from_slice(&raw).expect("session.json is not in the expected shape");
    assert!(
        !tokens.id_token.is_empty(),
        "session.json carries no idToken; log in with cs-tui first"
    );
    let client = Client::builder().build().expect("client");
    client.set_tokens(tokens).await;
    client
}

fn session_path() -> PathBuf {
    if let Ok(p) = std::env::var("CS_TUI_SESSION") {
        return PathBuf::from(p);
    }
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").expect("HOME")).join(".config"));
    config.join("cs-tui").join("session.json")
}

/// Does `PATCH /v1/settings` MERGE a partial `notifications` object into the
/// stored one, or REPLACE it wholesale?
///
/// This is the one open question with a live bug behind it. `SettingsUpdate`
/// sends only the keys the user toggled — `{"notifications":{"bookmark":false}}`
/// — and starts from `NotificationPrefs::default()`, so `reply`, `poke` and
/// every key the server keeps in `extra` are absent from the request. If the
/// server replaces, toggling one preference silently switches the others off.
///
/// § Update Settings shows an example sending all three keys together and never
/// says what a partial object does, so only the server can answer.
///
/// Touches one real setting and puts it back. Two writes fit inside the 2/min
/// budget, so it returns in a couple of seconds.
///
/// ANSWERED 2026-09-17: MERGE. A partial object is merged into the stored one —
/// seven keys the client does not model (`chat_mention`, `dm_message`,
/// `new_follower`, `new_post_following`, `new_post_friend`, `thread_reply`,
/// `unfollowed`) all survived a patch that named only `bookmark`. Kept so the
/// answer can be re-checked when the API moves.
#[tokio::test]
#[ignore = "talks to the live API as you"]
async fn settings_notifications_merge_or_replace() {
    let client = signed_in().await;

    let before = client.get_settings().await.expect("read settings");
    let prefs = before.notifications.clone();
    println!("stored notifications: {prefs:?}");
    assert!(
        prefs.reply.is_some() || prefs.poke.is_some() || !prefs.extra.is_empty(),
        "this probe needs at least one notification key set besides `bookmark`.\n\
         Toggle `notify on reply` in Settings, save, and run it again."
    );

    // Send `bookmark` alone, inverted, exactly as the settings screen would.
    let flipped = !prefs.bookmark.unwrap_or(true);
    let update = SettingsUpdate {
        notifications: Some(cs_api::NotificationPrefs {
            bookmark: Some(flipped),
            ..Default::default()
        }),
        ..Default::default()
    };
    client.update_settings(&update).await.expect("patch");

    let after = client.get_settings().await.expect("re-read settings");
    let got = after.notifications.clone();
    println!("after a partial patch: {got:?}");

    let survived = (prefs.reply.is_none() || got.reply == prefs.reply)
        && (prefs.poke.is_none() || got.poke == prefs.poke)
        && prefs.extra.keys().all(|k| got.extra.contains_key(k));

    // Put it back before asserting, so a REPLACE verdict does not also leave the
    // account with the other preferences switched off.
    let restore = SettingsUpdate {
        notifications: Some(prefs.clone()),
        ..Default::default()
    };
    client.update_settings(&restore).await.expect("restore");
    println!("restored");

    if survived {
        println!(
            "VERDICT: MERGE. A partial `notifications` object is merged into the \
             stored one. `SettingsUpdate` sending only dirty keys is correct."
        );
    } else {
        println!(
            "VERDICT: REPLACE. A partial `notifications` object REPLACES the stored \
             one — sending only the dirty keys drops the rest.\n\
             FIX: `SettingsScreen::build_update` must start from the `NotificationPrefs` \
             it loaded (including `extra`) and set the dirty key on top of it."
        );
    }
    assert!(survived, "see the VERDICT above — this is a real bug");
}

/// What unit does the server count a program description in — Unicode scalars,
/// UTF-16 code units, or bytes?
///
/// § Publish says "max 256 characters" without naming a unit. Both client checks
/// use `chars().count()` (scalars). A JS server enforcing `.length` counts UTF-16
/// units, where an emoji is 2. If so, a 200-emoji description passes locally and
/// is rejected server-side, spending one of three publishes a minute.
///
/// **Side effect**: this publishes to the PUBLIC gallery if the server accepts a
/// description, then purges it. It is ordered so the rejection cases create
/// nothing at all, and at most one program ever exists, for under a second.
#[tokio::test]
#[ignore = "talks to the live API as you, and may briefly publish"]
async fn program_description_length_unit() {
    let client = signed_in().await;
    let name = "cs-tui-probe";
    let source = b"export default async () => 0\n".to_vec();

    // 200 rockets: 200 scalars, 400 UTF-16 units, 800 bytes.
    let emoji = "\u{1F680}".repeat(200);
    // 200 hiragana: 200 scalars, 200 UTF-16 units, 600 bytes.
    let kana = "\u{3042}".repeat(200);

    let verdict = match try_publish(&client, name, &emoji, &source).await {
        Ok(id) => {
            purge(&client, &id).await;
            "SCALARS. 200 emoji (400 UTF-16 units) were accepted, so `chars().count()` \
             is the right unit and the client checks are correct."
        }
        Err(first) => {
            println!("200 emoji rejected: {first}");
            match try_publish(&client, name, &kana, &source).await {
                Ok(id) => {
                    purge(&client, &id).await;
                    "UTF-16 CODE UNITS. 200 emoji (400 units) were refused and 200 kana \
                     (200 units, 600 bytes) were accepted.\n\
                     FIX: count `chars().map(char::len_utf16).sum()` in \
                     `NewProgram::validate` and `PublishForm::validate`, so a \
                     description the server will refuse is caught before it spends \
                     a publish."
                }
                Err(second) => {
                    println!("200 kana rejected: {second}");
                    "BYTES. Both were refused, so the limit is on UTF-8 bytes.\n\
                     FIX: count `s.len()` in both validators."
                }
            }
        }
    };
    println!("VERDICT: {verdict}");
}

async fn try_publish(
    client: &Client,
    name: &str,
    description: &str,
    source: &[u8],
) -> Result<String, String> {
    let program = NewProgram::new(name, description, source.to_vec(), Runtime::Term);
    match client.publish_program(&program).await {
        Ok(p) => Ok(p.id),
        Err(e) => Err(e.to_string()),
    }
}

/// Remove the probe's program and the slot it holds, loudly if it fails: a
/// leftover in a public gallery is worth shouting about.
async fn purge(client: &Client, id: &str) {
    match client.purge_program(id).await {
        Ok(_) => println!("purged {id}"),
        Err(e) => eprintln!("!! could not purge {id}: {e} — remove it by hand"),
    }
}

/// Does `GET /v1/programs?author=<name>` filter on its own, or is `name`
/// required alongside it?
///
/// § Browse the Gallery only ever shows the pair. `ProgramQuery::pairs` drops a
/// lone `author` rather than sending it, on the reasoning that a half-named
/// lookup answered with the whole gallery is worse than no lookup. If the server
/// honours `author` alone, that reasoning is wrong and the client is throwing
/// away a useful listing.
///
/// Also checks the documented pair (`?author=&name=`) actually works, since
/// [`ProgramQuery::by_author_and_name`] is a constructor this crate ships and
/// nothing else exercises.
///
/// ANSWERED 2026-09-18: a lone `author` is IGNORED — 27 programs came back
/// either way, unfiltered. Dropping it client-side matches the server, so the
/// current behaviour is right. The pair WORKS: `?author=&name=` resolved to
/// exactly the one program, so [`ProgramQuery::by_author_and_name`] models a
/// real server feature and is worth keeping even with no caller in the TUI.
///
/// Two or three reads, no side effects.
#[tokio::test]
#[ignore = "talks to the live API as you"]
async fn programs_author_filter_alone() {
    let client = signed_in().await;

    let (all, _) = client
        .list_programs(&ProgramQuery::default(), None, Some(50))
        .await
        .expect("gallery");
    let Some(author) = all.first().map(|p| p.owner_username.clone()) else {
        println!("VERDICT: INCONCLUSIVE — the gallery is empty, so there is nobody to filter on.");
        return;
    };
    let others = all.iter().filter(|p| p.owner_username != author).count();
    println!(
        "gallery: {} programs, {others} by someone other than @{author}",
        all.len()
    );
    if others == 0 {
        println!(
            "VERDICT: INCONCLUSIVE — every program in the gallery is @{author}'s, so a \
             filtered and an unfiltered page look identical."
        );
        return;
    }

    // Send `author` with no `name`, which `ProgramQuery` will not build.
    let probe = ProgramQuery {
        author: Some(author.clone()),
        ..Default::default()
    };
    let (filtered, _) = client
        .list_programs(&probe, None, Some(50))
        .await
        .expect("filtered");
    let foreign = filtered
        .iter()
        .filter(|p| p.owner_username != author)
        .count();
    println!(
        "with ?author=@{author}: {} programs, {foreign} by others",
        filtered.len()
    );

    if foreign == 0 && !filtered.is_empty() {
        println!(
            "VERDICT (lone author): HONOURED. `author` filters on its own.\n\
             FIX: `ProgramQuery::pairs` should send a lone `author` instead of dropping it."
        );
    } else {
        println!(
            "VERDICT (lone author): IGNORED. A lone `author` came back unfiltered, so \
             dropping it client-side matches what the server does. It also means the \
             half-named form is a silent trap for a caller building the struct by \
             hand: asking for one person's program answers with everybody's."
        );
    }

    // And the documented form, which is what `by_author_and_name` builds.
    let target = all
        .iter()
        .find(|p| p.owner_username == author)
        .expect("the author came from this list");
    let (pair, _) = client
        .list_programs(
            &ProgramQuery::by_author_and_name(&author, &target.name),
            None,
            Some(50),
        )
        .await
        .expect("author+name lookup");
    println!(
        "with ?author=@{author}&name={}: {} programs",
        target.name,
        pair.len()
    );
    let resolved = pair.len() == 1 && pair[0].id == target.id;
    if resolved {
        println!(
            "VERDICT (author+name): WORKS. The documented lookup returns exactly that program."
        );
    } else {
        println!(
            "VERDICT (author+name): DOES NOT RESOLVE. `ProgramQuery::by_author_and_name` \
             ships a form the server does not honour — either fix the query or drop the \
             constructor rather than leaving a lookup that quietly answers with a page."
        );
    }
}
