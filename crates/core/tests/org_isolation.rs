//! Several orgs share one database. Nothing an org stores may be visible to,
//! or changed by, another org.
//!
//! Two guards live here: a behavioural test that runs two orgs side by side on
//! one SQLite file, and a lint over the SQL text in the storage layer that
//! fails when a statement touches a tenant table without scoping it to an org.

/// Every table that holds tenant data. A new table must be added here.
const TENANT_TABLES: &[&str] = &[
    "identities",
    "boards",
    "board_members",
    "posts",
    "handling",
    "agents",
    "subscriptions",
    "deliveries",
    "cursors",
    "meta",
];

const SOURCES: &[(&str, &str)] = &[
    ("authority.rs", include_str!("../src/authority.rs")),
    ("inbox.rs", include_str!("../src/inbox.rs")),
];

// ---- lint -------------------------------------------------------------------

/// The contents of every ordinary string literal in a Rust source file.
fn string_literals(src: &str) -> Vec<String> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '/' if c.get(i + 1) == Some(&'/') => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            // Character literals, so that '"' does not open a string. Lifetimes fall through.
            '\'' if c.get(i + 1) == Some(&'\\') => i += 4,
            '\'' if c.get(i + 2) == Some(&'\'') => i += 3,
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' {
                        i += 1;
                    }
                    s.push(c[i]);
                    i += 1;
                }
                i += 1;
                out.push(s);
            }
            _ => i += 1,
        }
    }
    out
}

fn words(s: &str) -> Vec<&str> {
    s.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|w| !w.is_empty())
        .collect()
}

/// SQL statements (and fragments holding a subquery) that read or write tenant tables.
fn tenant_statements(src: &str) -> Vec<String> {
    string_literals(src)
        .into_iter()
        .filter(|s| {
            let w = words(s);
            w.iter()
                .any(|x| ["SELECT", "INSERT", "UPDATE", "DELETE"].contains(x))
                && w.iter().any(|x| TENANT_TABLES.contains(x))
        })
        .collect()
}

#[test]
fn every_statement_on_a_tenant_table_is_scoped_to_an_org() {
    let mut seen = 0;
    let mut bad = Vec::new();
    for (file, src) in SOURCES {
        for stmt in tenant_statements(src) {
            seen += 1;
            let w = words(&stmt);
            let tables = w.iter().filter(|x| TENANT_TABLES.contains(x)).count();
            let orgs = w.iter().filter(|x| **x == "org").count();
            // Each table a statement names needs its own org condition (or org
            // column, for an insert): one `org` for the outer query is not
            // enough when a subquery reads a second table.
            if orgs < tables {
                bad.push(format!("{file}: names {tables} tenant table(s) but mentions org {orgs} time(s):\n    {stmt}"));
            }
        }
    }
    assert!(seen >= 50, "the lint found only {seen} statements; has the SQL moved out of authority.rs and inbox.rs?");
    assert!(
        bad.is_empty(),
        "SQL that is not scoped to an org:\n\n{}\n",
        bad.join("\n\n")
    );
}

#[test]
fn every_table_has_an_org_column() {
    let mut tables = Vec::new();
    for (file, src) in SOURCES {
        for lit in string_literals(src) {
            for def in lit.split("CREATE TABLE IF NOT EXISTS ").skip(1) {
                let name = words(def)[0];
                let body = def.split(';').next().unwrap();
                assert!(
                    body.contains("org TEXT NOT NULL"),
                    "{file}: table {name} has no org column"
                );
                assert!(
                    body.contains("PRIMARY KEY (org, ") || body.contains("UNIQUE (org, "),
                    "{file}: table {name} has no key that leads with org"
                );
                tables.push(name.to_string());
            }
        }
    }
    tables.sort();
    let mut expected: Vec<String> = TENANT_TABLES.iter().map(|t| t.to_string()).collect();
    expected.sort();
    assert_eq!(
        tables, expected,
        "TENANT_TABLES in this test must list every table the schema creates"
    );
}

#[test]
fn the_lint_catches_unscoped_sql() {
    let src = r#"
        fn f() {
            // "SELECT * FROM posts" in a comment is ignored
            let c = '"';
            a("SELECT * FROM posts WHERE id = ?");
            b("SELECT 1 FROM posts p WHERE p.org = ? AND EXISTS (SELECT 1 FROM handling h WHERE h.post = p.id)");
            c("DELETE FROM subscriptions WHERE org = ? AND agent = ?");
            d("you can only retract your own posts");
        }
    "#;
    let found = tenant_statements(src);
    assert_eq!(found.len(), 3, "{found:?}");
    let scoped = |s: &str| {
        words(s).iter().filter(|x| **x == "org").count()
            >= words(s)
                .iter()
                .filter(|x| TENANT_TABLES.contains(x))
                .count()
    };
    assert!(!scoped(&found[0]));
    assert!(
        !scoped(&found[1]),
        "a subquery on a second table needs its own org condition"
    );
    assert!(scoped(&found[2]));
}

// ---- behaviour --------------------------------------------------------------

#[cfg(feature = "native")]
mod shared_database {
    use std::path::PathBuf;

    use backchannel_core::authority::{Authority, BoardSpec, Ingest, Mode, Profile, ReadOpts};
    use backchannel_core::crypto::{iso, sign_envelope, ulid, KeyPair};
    use backchannel_core::inbox::{AgentInfo, Inbox};
    use backchannel_core::sql::{int, text, Sql};
    use backchannel_core::sqlite::Sqlite;
    use backchannel_core::tools::{call, Ctx, Draft, Me};
    use backchannel_core::*;
    use serde_json::json;

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    /// A database file that is removed when the test ends.
    struct TempDb(PathBuf);

    impl TempDb {
        fn new(tag: &str) -> TempDb {
            let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
                "org-isolation-{tag}-{}-{}.db",
                std::process::id(),
                now()
            ));
            TempDb(path)
        }

        /// One more connection to the same file, serving `org`.
        fn org(&self, org: &str, mode: Mode) -> Authority<Sqlite> {
            let mut auth = Authority::new(Sqlite::open(&self.0).unwrap(), mode, now).unwrap();
            auth.org = org.into();
            auth
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    fn person(name: &str) -> Me {
        Me {
            name: name.into(),
            keys: KeyPair::generate(),
        }
    }

    fn ctx<'a>(
        auth: &'a Authority<Sqlite>,
        inbox: &'a Inbox<'a, Sqlite>,
        me: &'a Me,
        label: &str,
    ) -> Ctx<'a, Sqlite> {
        Ctx {
            auth,
            inbox,
            me,
            label: label.into(),
            client: "test".into(),
        }
    }

    fn draft(board: &str, kind: Kind, body: &str) -> Draft {
        Draft {
            board: board.into(),
            body: body.into(),
            kind: Some(kind),
            ..Default::default()
        }
    }

    /// A signed post that has not been handed to any authority yet.
    fn envelope(
        me: &Me,
        agent: &str,
        board: &str,
        kind: Kind,
        body: &str,
        id: &str,
        ref_: Option<&str>,
    ) -> Envelope {
        let env = Envelope {
            v: PROTOCOL.into(),
            id: id.into(),
            board: board.into(),
            from: Author {
                name: me.name.clone(),
                agent: agent.into(),
                key: me.keys.public_key.clone(),
            },
            kind,
            body: body.into(),
            title: None,
            ref_: ref_.map(str::to_string),
            mentions: None,
            tags: None,
            data: None,
            lease: None,
            expires: None,
            ext: None,
            ts: iso(now()),
            sig: String::new(),
        };
        sign_envelope(env, &me.keys)
    }

    fn bodies(posts: &[StoredPost]) -> Vec<String> {
        posts.iter().map(|p| p.envelope.body.clone()).collect()
    }

    fn names(boards: &[Board]) -> Vec<String> {
        boards.iter().map(|b| b.name.clone()).collect()
    }

    #[test]
    fn default_org_is_local() {
        let auth = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, now).unwrap();
        assert_eq!(auth.org, "local");
        let attached = Authority::attach(Sqlite::memory().unwrap(), Mode::Authority, now);
        assert_eq!(attached.org, "local");
    }

    #[test]
    fn two_orgs_on_one_database_never_see_each_other() {
        let db = TempDb::new("main");
        let acme = db.org("acme", Mode::Authority);
        let globex = db.org("globex", Mode::Authority);
        let acme_inbox = Inbox::new(&acme, true, vec!["smoko".into()]).unwrap();
        let globex_inbox = Inbox::new(&globex, true, vec!["smoko".into()]).unwrap();

        // The same names in both orgs, bound to different keys.
        let (a_alice, a_bob, a_carol) = (person("alice"), person("bob"), person("carol"));
        let (g_alice, g_bob, g_carol) = (person("alice"), person("bob"), person("carol"));
        for p in [&a_alice, &a_bob, &a_carol] {
            acme.register(
                &p.name,
                &p.keys.public_key,
                Profile {
                    display_name: Some(format!("{} of acme", p.name)),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        // Only two of them exist in globex.
        for p in [&g_alice, &g_bob] {
            globex
                .register(
                    &p.name,
                    &p.keys.public_key,
                    Profile {
                        display_name: Some(format!("{} of globex", p.name)),
                        ..Default::default()
                    },
                )
                .unwrap();
        }

        // ---- identities ----
        assert_eq!(acme.identities().unwrap().len(), 3);
        assert_eq!(globex.identities().unwrap().len(), 2);
        assert!(acme
            .identities()
            .unwrap()
            .iter()
            .all(|i| i.display_name.ends_with("of acme")));
        assert!(globex
            .identities()
            .unwrap()
            .iter()
            .all(|i| i.display_name.ends_with("of globex")));
        assert_eq!(
            acme.identity("alice").unwrap().unwrap().key,
            a_alice.keys.public_key
        );
        assert_eq!(
            globex.identity("alice").unwrap().unwrap().key,
            g_alice.keys.public_key
        );
        assert!(globex.identity("carol").unwrap().is_none());
        // A key is only good in the org that bound it.
        acme.assert_key("alice", &a_alice.keys.public_key).unwrap();
        assert_eq!(
            globex
                .assert_key("alice", &a_alice.keys.public_key)
                .err()
                .unwrap()
                .code,
            Code::Unauthorized
        );
        assert_eq!(
            globex
                .assert_key("carol", &a_carol.keys.public_key)
                .err()
                .unwrap()
                .code,
            Code::Unauthorized
        );
        // Re-registering in one org leaves the other's profile alone.
        acme.register(
            "alice",
            &a_alice.keys.public_key,
            Profile {
                bio: Some("acme bio".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(globex.identity("alice").unwrap().unwrap().bio, "");
        // Globex's carol arrives later with her own key; acme's binding does not block her.
        globex
            .register("carol", &g_carol.keys.public_key, Profile::default())
            .unwrap();
        assert_eq!(
            acme.identity("carol").unwrap().unwrap().key,
            a_carol.keys.public_key
        );

        // ---- boards ----
        acme.ensure_board("smoko", "acme chat").unwrap();
        globex.ensure_board("smoko", "globex chat").unwrap();
        acme.create_board(
            "alice",
            BoardSpec {
                name: "eng".into(),
                description: Some("acme eng".into()),
                ..Default::default()
            },
        )
        .unwrap();
        globex
            .create_board(
                "bob",
                BoardSpec {
                    name: "eng".into(),
                    description: Some("globex eng".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // The same private board name, with different members: carol is in globex's only.
        acme.create_board(
            "alice",
            BoardSpec {
                name: "plans".into(),
                visibility: Some(Visibility::Private),
                ..Default::default()
            },
        )
        .unwrap();
        globex
            .create_board(
                "alice",
                BoardSpec {
                    name: "plans".into(),
                    visibility: Some(Visibility::Private),
                    members: Some(vec!["carol".into()]),
                    ..Default::default()
                },
            )
            .unwrap();
        acme.create_board(
            "alice",
            BoardSpec {
                name: "acme-only".into(),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(acme.board("eng").unwrap().unwrap().description, "acme eng");
        assert_eq!(
            globex.board("eng").unwrap().unwrap().description,
            "globex eng"
        );
        assert_eq!(acme.board("eng").unwrap().unwrap().created_by, "alice");
        assert_eq!(globex.board("eng").unwrap().unwrap().created_by, "bob");
        assert!(globex.board("acme-only").unwrap().is_none());
        assert_eq!(
            names(&acme.boards(None).unwrap()),
            ["acme-only", "eng", "plans", "smoko"]
        );
        assert_eq!(
            names(&globex.boards(None).unwrap()),
            ["eng", "plans", "smoko"]
        );
        assert_eq!(
            acme.board("plans").unwrap().unwrap().members.unwrap(),
            ["alice"]
        );
        assert_eq!(
            globex.board("plans").unwrap().unwrap().members.unwrap(),
            ["alice", "carol"]
        );
        // Membership does not cross over: carol is a member of globex's plans only.
        assert!(!acme.can_read(Some("carol"), "plans", None).unwrap());
        assert!(globex.can_read(Some("carol"), "plans", None).unwrap());
        assert!(!names(&acme.boards(Some("carol")).unwrap()).contains(&"plans".to_string()));
        assert!(names(&globex.boards(Some("carol")).unwrap()).contains(&"plans".to_string()));
        assert!(!globex.can_read(Some("alice"), "acme-only", None).unwrap());

        // ---- agents and subscriptions ----
        let mut aa = ctx(&acme, &acme_inbox, &a_alice, "dev");
        let mut ab = ctx(&acme, &acme_inbox, &a_bob, "dev");
        let mut ac = ctx(&acme, &acme_inbox, &a_carol, "dev");
        let mut ga = ctx(&globex, &globex_inbox, &g_alice, "dev");
        let mut gb = ctx(&globex, &globex_inbox, &g_bob, "dev");
        let mut gc = ctx(&globex, &globex_inbox, &g_carol, "dev");
        for c in [&mut aa, &mut ab, &mut ac, &mut ga, &mut gb, &mut gc] {
            call(c, "whoami", &json!({})).unwrap();
        }
        acme_inbox
            .touch_agent(
                "alice",
                "dev",
                AgentInfo {
                    status: Some("acme status".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        acme_inbox
            .touch_agent("bob", "acme-extra", AgentInfo::default())
            .unwrap();
        acme_inbox.subscribe("bob/dev", "eng", None).unwrap();
        acme_inbox.subscribe("carol/dev", "**", None).unwrap();
        globex_inbox
            .subscribe("bob/dev", "eng", Some(vec![Kind::Ask]))
            .unwrap();
        globex_inbox.subscribe("carol/dev", "**", None).unwrap();
        globex_inbox.subscribe("alice/dev", "plans", None).unwrap();

        let addresses = |i: &Inbox<Sqlite>| {
            let mut v: Vec<String> = i
                .agents(None)
                .unwrap()
                .into_iter()
                .map(|a| a.address)
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            addresses(&acme_inbox),
            ["alice/dev", "bob/acme-extra", "bob/dev", "carol/dev"]
        );
        assert_eq!(
            addresses(&globex_inbox),
            ["alice/dev", "bob/dev", "carol/dev"]
        );
        assert_eq!(
            acme_inbox.agents(Some("alice")).unwrap()[0].status,
            "acme status"
        );
        assert_eq!(globex_inbox.agents(Some("alice")).unwrap()[0].status, "");
        let patterns = |i: &Inbox<Sqlite>, a: &str| {
            i.subscriptions(a)
                .unwrap()
                .into_iter()
                .map(|s| s.pattern)
                .collect::<Vec<_>>()
        };
        assert_eq!(patterns(&acme_inbox, "bob/dev"), ["eng", "smoko"]);
        assert_eq!(patterns(&globex_inbox, "alice/dev"), ["plans", "smoko"]);
        assert_eq!(patterns(&acme_inbox, "alice/dev"), ["smoko"]);
        assert!(
            acme_inbox
                .subscriptions("bob/dev")
                .unwrap()
                .iter()
                .all(|s| s.kinds.is_none()),
            "globex's kind filter leaked into acme"
        );
        // Unsubscribing in one org leaves the other's subscription in place.
        globex_inbox.unsubscribe("bob/dev", "smoko").unwrap();
        assert_eq!(patterns(&acme_inbox, "bob/dev"), ["eng", "smoko"]);
        assert_eq!(patterns(&globex_inbox, "bob/dev"), ["eng"]);

        // ---- posts: interleaved, so the shared gseq alternates between orgs ----
        let a1 = aa.publish(draft("eng", Kind::Ask, "acme ask 1")).unwrap();
        let g1 = ga.publish(draft("eng", Kind::Ask, "globex ask 1")).unwrap();
        let a2 = aa.publish(draft("eng", Kind::Note, "acme note 2")).unwrap();
        let a3 = aa.publish(draft("eng", Kind::Task, "acme task 3")).unwrap();
        let g2 = ga
            .publish(draft("eng", Kind::Note, "globex note 2"))
            .unwrap();
        let a_plan = aa
            .publish(draft("plans", Kind::Note, "acme secret plan"))
            .unwrap();
        let g_plan = ga
            .publish(draft("plans", Kind::Note, "globex secret plan"))
            .unwrap();
        let a_dm = ab
            .publish(draft("@alice", Kind::Note, "acme dm to alice"))
            .unwrap();
        let g_dm = gb
            .publish(draft("@alice", Kind::Note, "globex dm to alice"))
            .unwrap();

        // Board sequence numbers are per org.
        assert_eq!([a1.seq, a2.seq, a3.seq], [1, 2, 3]);
        assert_eq!([g1.seq, g2.seq], [1, 2]);
        assert_eq!([a_plan.seq, g_plan.seq, a_dm.seq, g_dm.seq], [1, 1, 1, 1]);
        assert_eq!(acme.board("eng").unwrap().unwrap().head, 3);
        assert_eq!(globex.board("eng").unwrap().unwrap().head, 2);
        // The global order is shared and strictly increasing across both orgs.
        let all = [&a1, &g1, &a2, &a3, &g2, &a_plan, &g_plan, &a_dm, &g_dm];
        assert!(
            all.windows(2).all(|w| w[0].gseq < w[1].gseq),
            "{:?}",
            all.iter().map(|p| p.gseq).collect::<Vec<_>>()
        );
        assert_eq!(acme.head().unwrap(), a_dm.gseq);
        assert_eq!(globex.head().unwrap(), g_dm.gseq);

        // ---- feed ----
        let acme_feed = acme.feed(None, 0, 200, true).unwrap();
        let globex_feed = globex.feed(None, 0, 200, true).unwrap();
        assert_eq!(
            bodies(&acme_feed),
            [
                "acme ask 1",
                "acme note 2",
                "acme task 3",
                "acme secret plan",
                "acme dm to alice"
            ]
        );
        assert_eq!(
            bodies(&globex_feed),
            [
                "globex ask 1",
                "globex note 2",
                "globex secret plan",
                "globex dm to alice"
            ]
        );
        assert!(bodies(&acme.feed(Some("alice"), 0, 200, true).unwrap())
            .iter()
            .all(|b| b.starts_with("acme")));
        assert!(bodies(&globex.feed(Some("alice"), 0, 200, true).unwrap())
            .iter()
            .all(|b| b.starts_with("globex")));
        // Paging by gseq stays inside the org even though the numbers interleave.
        assert_eq!(
            bodies(&acme.feed(None, g1.gseq, 200, true).unwrap())[0],
            "acme note 2"
        );
        // Private membership: carol belongs to globex's plans, not acme's.
        assert_eq!(
            bodies(&acme.feed(Some("carol"), 0, 200, true).unwrap()),
            ["acme ask 1", "acme note 2", "acme task 3"]
        );
        assert_eq!(
            bodies(&globex.feed(Some("carol"), 0, 200, true).unwrap()),
            ["globex ask 1", "globex note 2", "globex secret plan"]
        );
        // Direct messages: each alice sees only her own org's.
        assert_eq!(
            bodies(
                &acme
                    .read_board(Some("alice"), "@alice", ReadOpts::default())
                    .unwrap()
            ),
            ["acme dm to alice"]
        );
        assert_eq!(
            bodies(
                &globex
                    .read_board(Some("alice"), "@alice", ReadOpts::default())
                    .unwrap()
            ),
            ["globex dm to alice"]
        );
        assert_eq!(
            names(&acme.boards(None).unwrap())
                .iter()
                .filter(|n| n.starts_with('@'))
                .count(),
            1
        );

        // ---- read_board ----
        assert_eq!(
            bodies(&acme.read_board(None, "eng", ReadOpts::default()).unwrap()),
            ["acme ask 1", "acme note 2", "acme task 3"]
        );
        assert_eq!(
            bodies(&globex.read_board(None, "eng", ReadOpts::default()).unwrap()),
            ["globex ask 1", "globex note 2"]
        );
        assert_eq!(
            bodies(
                &globex
                    .read_board(
                        Some("bob"),
                        "eng",
                        ReadOpts {
                            after: Some(1),
                            ..Default::default()
                        }
                    )
                    .unwrap()
            ),
            ["globex note 2"]
        );
        assert_eq!(
            bodies(
                &acme
                    .read_board(
                        Some("bob"),
                        "eng",
                        ReadOpts {
                            before: Some(3),
                            ..Default::default()
                        }
                    )
                    .unwrap()
            ),
            ["acme ask 1", "acme note 2"]
        );
        assert_eq!(
            globex
                .read_board(Some("alice"), "acme-only", ReadOpts::default())
                .err()
                .unwrap()
                .code,
            Code::NotFound
        );
        assert_eq!(
            acme.read_board(Some("carol"), "plans", ReadOpts::default())
                .err()
                .unwrap()
                .code,
            Code::NotFound
        );
        assert_eq!(
            bodies(
                &globex
                    .read_board(Some("carol"), "plans", ReadOpts::default())
                    .unwrap()
            ),
            ["globex secret plan"]
        );

        // ---- post(id) and thread ----
        let a1_id = a1.envelope.id.as_str();
        let g1_id = g1.envelope.id.as_str();
        assert_eq!(
            acme.post(a1_id).unwrap().unwrap().envelope.body,
            "acme ask 1"
        );
        assert!(globex.post(a1_id).unwrap().is_none());
        assert!(acme.post(g1_id).unwrap().is_none());
        assert_eq!(
            globex.thread(None, a1_id, true).err().unwrap().code,
            Code::NotFound
        );
        assert_eq!(
            globex
                .thread(Some("alice"), a1_id, true)
                .err()
                .unwrap()
                .code,
            Code::NotFound
        );

        // ---- an acme post cannot be replied to, claimed or handled from globex ----
        let reply = envelope(
            &g_bob,
            "dev",
            "eng",
            Kind::Reply,
            "globex reply to an acme post",
            &ulid(now()),
            Some(a1_id),
        );
        assert_eq!(globex.accept(reply).err().unwrap().code, Code::NotFound);
        let claim = envelope(
            &g_bob,
            "dev",
            "eng",
            Kind::Claim,
            "",
            &ulid(now()),
            Some(a1_id),
        );
        assert_eq!(globex.accept(claim).err().unwrap().code, Code::NotFound);
        let retract = envelope(
            &g_alice,
            "dev",
            "eng",
            Kind::Retract,
            "",
            &ulid(now()),
            Some(a1_id),
        );
        assert_eq!(globex.accept(retract).err().unwrap().code, Code::NotFound);
        for tool in ["claim", "handled", "release", "retract"] {
            assert_eq!(
                call(&mut gb, tool, &json!({ "id": a1_id }))
                    .err()
                    .unwrap()
                    .code,
                Code::NotFound,
                "{tool}"
            );
        }
        assert_eq!(
            call(&mut gb, "post", &json!({ "ref": a1_id, "body": "sneaky" }))
                .err()
                .unwrap()
                .code,
            Code::NotFound
        );
        assert_eq!(
            call(&mut gb, "read", &json!({ "thread": a1_id }))
                .err()
                .unwrap()
                .code,
            Code::NotFound
        );
        // An acme key cannot post into globex under the same name.
        let imposter = envelope(
            &a_alice,
            "dev",
            "eng",
            Kind::Note,
            "acme alice in globex",
            &ulid(now()),
            None,
        );
        assert_eq!(globex.accept(imposter).err().unwrap().code, Code::Forbidden);
        // None of that left a trace in either org.
        assert_eq!(acme.feed(None, 0, 200, true).unwrap().len(), 5);
        assert_eq!(globex.feed(None, 0, 200, true).unwrap().len(), 4);
        assert_eq!(acme.handling(a1_id).unwrap().state, HandlingState::Open);
        assert!(!acme.post(a1_id).unwrap().unwrap().retracted);

        // ---- replies, claims and handling inside each org ----
        ab.publish(Draft {
            board: "eng".into(),
            body: "acme reply".into(),
            ref_: Some(a1_id.into()),
            ..Default::default()
        })
        .unwrap();
        gb.publish(Draft {
            board: "eng".into(),
            body: "globex reply".into(),
            ref_: Some(g1_id.into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            bodies(&acme.thread(None, a1_id, true).unwrap()),
            ["acme ask 1", "acme reply"]
        );
        assert_eq!(
            bodies(&globex.thread(Some("bob"), g1_id, true).unwrap()),
            ["globex ask 1", "globex reply"]
        );

        call(&mut ab, "claim", &json!({ "id": a1_id })).unwrap();
        assert_eq!(acme.handling(a1_id).unwrap().state, HandlingState::Claimed);
        assert_eq!(
            acme.handling(a1_id).unwrap().holder.as_deref(),
            Some("bob/dev")
        );
        assert_eq!(globex.handling(a1_id).unwrap().state, HandlingState::Open);
        assert_eq!(globex.handling(g1_id).unwrap().state, HandlingState::Open);
        call(&mut ab, "handled", &json!({ "id": a1_id })).unwrap();
        assert_eq!(acme.handling(a1_id).unwrap().state, HandlingState::Handled);
        assert_eq!(globex.handling(a1_id).unwrap().state, HandlingState::Open);

        // ---- open_items ----
        let open = |a: &Authority<Sqlite>, v: Option<&str>| {
            a.open_items(v, 50)
                .unwrap()
                .into_iter()
                .map(|(p, _)| p.envelope.body)
                .collect::<Vec<_>>()
        };
        assert_eq!(open(&acme, None), ["acme task 3"]);
        assert_eq!(open(&globex, None), ["globex ask 1"]);
        assert_eq!(open(&acme, Some("carol")), ["acme task 3"]);
        assert_eq!(open(&globex, Some("carol")), ["globex ask 1"]);

        // ---- the same post id in both orgs ----
        let shared = ulid(now());
        acme.accept(envelope(
            &a_alice,
            "dev",
            "smoko",
            Kind::Ask,
            "acme twin",
            &shared,
            None,
        ))
        .unwrap();
        let twin = globex
            .accept(envelope(
                &g_alice,
                "dev",
                "smoko",
                Kind::Ask,
                "globex twin",
                &shared,
                None,
            ))
            .unwrap();
        assert_eq!(twin.envelope.body, "globex twin");
        assert_eq!(
            acme.post(&shared).unwrap().unwrap().envelope.body,
            "acme twin"
        );
        assert_eq!(
            globex.post(&shared).unwrap().unwrap().envelope.body,
            "globex twin"
        );
        acme_inbox.fanout().unwrap();
        globex_inbox.fanout().unwrap();
        // Claiming, handling and retracting one twin does nothing to the other.
        call(&mut ab, "claim", &json!({ "id": shared })).unwrap();
        assert_eq!(
            acme.handling(&shared).unwrap().state,
            HandlingState::Claimed
        );
        assert_eq!(globex.handling(&shared).unwrap().state, HandlingState::Open);
        call(&mut gb, "claim", &json!({ "id": shared })).unwrap();
        call(&mut gb, "handled", &json!({ "id": shared })).unwrap();
        assert_eq!(
            globex.handling(&shared).unwrap().state,
            HandlingState::Handled
        );
        assert_eq!(
            acme.handling(&shared).unwrap().state,
            HandlingState::Claimed
        );
        assert_eq!(
            acme.handling(&shared).unwrap().holder.as_deref(),
            Some("bob/dev")
        );
        call(&mut ab, "release", &json!({ "id": shared })).unwrap();
        call(&mut aa, "retract", &json!({ "id": shared })).unwrap();
        assert!(acme.post(&shared).unwrap().unwrap().retracted);
        assert!(!globex.post(&shared).unwrap().unwrap().retracted);
        assert_eq!(
            globex.handling(&shared).unwrap().state,
            HandlingState::Handled
        );
        assert_eq!(
            bodies(&acme.thread(None, &shared, true).unwrap())[0],
            "acme twin"
        );
        assert_eq!(
            bodies(&globex.thread(None, &shared, true).unwrap())[0],
            "globex twin"
        );
        assert!(
            bodies(&globex.thread(None, &shared, true).unwrap()).len() == 3,
            "globex twin, its claim and its handled"
        );

        // ---- inboxes ----
        let read = |i: &Inbox<Sqlite>, a: &str| i.read(a, 100, false).unwrap().0;
        for (inbox, org, other) in [
            (&acme_inbox, "acme", "globex"),
            (&globex_inbox, "globex", "acme"),
        ] {
            for agent in ["alice/dev", "bob/dev", "carol/dev"] {
                let unread = inbox.unread(agent).unwrap();
                let (peeked, _) = inbox.read(agent, 100, true).unwrap();
                assert_eq!(peeked.len() as i64, unread, "{org} {agent}");
                let items = read(inbox, agent);
                assert!(!items.is_empty(), "{org} {agent} got nothing");
                for d in &items {
                    let source = if d.post.envelope.body.is_empty() {
                        inbox
                            .auth
                            .post(d.post.envelope.ref_.as_deref().unwrap())
                            .unwrap()
                            .unwrap()
                            .envelope
                            .body
                    } else {
                        d.post.envelope.body.clone()
                    };
                    assert!(
                        source.starts_with(org) && !source.contains(other),
                        "{org} {agent} was delivered: {source}"
                    );
                }
                assert_eq!(inbox.unread(agent).unwrap(), 0);
            }
        }
        // Reading one org's inbox did not consume the same address's inbox in the other:
        // new mail for globex's bob, while acme's bob stays at zero.
        ga.publish(draft("eng", Kind::Ask, "globex ask for bob"))
            .unwrap();
        assert_eq!(globex_inbox.unread("bob/dev").unwrap(), 1);
        assert_eq!(acme_inbox.unread("bob/dev").unwrap(), 0);
        acme_inbox.ack("bob/dev", i64::MAX / 2).unwrap();
        assert_eq!(
            globex_inbox.unread("bob/dev").unwrap(),
            1,
            "acking in acme moved globex's cursor"
        );
        assert_eq!(
            bodies(
                &read(&globex_inbox, "bob/dev")
                    .into_iter()
                    .map(|d| d.post)
                    .collect::<Vec<_>>()
            ),
            ["globex ask for bob"]
        );

        // ---- the fan-out cursor is per org ----
        // Globex accepts a post but has not fanned out yet; acme then accepts a later
        // post and fans out. Acme's cursor passing that gseq must not make globex skip it.
        let early = globex
            .accept(envelope(
                &g_alice,
                "dev",
                "eng",
                Kind::Ask,
                "globex early",
                &ulid(now()),
                None,
            ))
            .unwrap();
        let late = acme
            .accept(envelope(
                &a_alice,
                "dev",
                "eng",
                Kind::Ask,
                "acme late",
                &ulid(now()),
                None,
            ))
            .unwrap();
        assert!(early.gseq < late.gseq);
        let mut touched = acme_inbox.fanout().unwrap();
        touched.sort();
        assert_eq!(touched, ["bob/dev", "carol/dev"]);
        assert_eq!(globex_inbox.unread("bob/dev").unwrap(), 0);
        let mut touched = globex_inbox.fanout().unwrap();
        touched.sort();
        assert_eq!(touched, ["bob/dev", "carol/dev"]);
        assert_eq!(
            bodies(
                &read(&globex_inbox, "bob/dev")
                    .into_iter()
                    .map(|d| d.post)
                    .collect::<Vec<_>>()
            ),
            ["globex early"]
        );
        assert_eq!(
            bodies(
                &read(&acme_inbox, "carol/dev")
                    .into_iter()
                    .map(|d| d.post)
                    .collect::<Vec<_>>()
            ),
            ["acme late"]
        );
        // Running fan-out again re-delivers nothing in either org.
        assert!(acme_inbox.fanout().unwrap().is_empty());
        assert!(globex_inbox.fanout().unwrap().is_empty());
        // The sync cursor is per org too.
        acme_inbox.set_sync_cursor(41).unwrap();
        assert_eq!(acme_inbox.sync_cursor().unwrap(), 41);
        assert_eq!(globex_inbox.sync_cursor().unwrap(), 0);

        // ---- a new agent's backfill only looks at its own org ----
        gb.publish(draft("@carol", Kind::Note, "globex dm to carol"))
            .unwrap();
        acme_inbox
            .touch_agent("carol", "second", AgentInfo::default())
            .unwrap();
        assert_eq!(acme_inbox.unread("carol/second").unwrap(), 0);
        globex_inbox
            .touch_agent("carol", "second", AgentInfo::default())
            .unwrap();
        assert_eq!(
            bodies(
                &read(&globex_inbox, "carol/second")
                    .into_iter()
                    .map(|d| d.post)
                    .collect::<Vec<_>>()
            ),
            ["globex dm to carol"]
        );

        // ---- a third org on the same file starts empty ----
        let initech = db.org("initech", Mode::Authority);
        let initech_inbox = Inbox::new(&initech, true, vec![]).unwrap();
        assert!(initech.identities().unwrap().is_empty());
        assert!(initech.boards(None).unwrap().is_empty());
        assert!(initech.feed(None, 0, 200, true).unwrap().is_empty());
        assert!(initech.open_items(None, 50).unwrap().is_empty());
        assert!(initech_inbox.agents(None).unwrap().is_empty());
        assert_eq!(initech.head().unwrap(), 0);
        assert!(initech.post(a1_id).unwrap().is_none());
        assert!(initech_inbox.fanout().unwrap().is_empty());

        // ---- and it really was one database: the raw rows hold every org ----
        let raw = Sqlite::open(&db.0).unwrap();
        for table in super::TENANT_TABLES {
            let rows = raw
                .query(
                    &format!("SELECT org, COUNT(*) AS n FROM {table} GROUP BY org ORDER BY org"),
                    &[],
                )
                .unwrap();
            let orgs: Vec<String> = rows.iter().map(|r| text(r, "org")).collect();
            assert_eq!(orgs, ["acme", "globex"], "{table}");
            assert!(rows.iter().all(|r| int(r, "n") > 0));
        }
        let total = int(
            &raw.query("SELECT COUNT(*) AS n FROM posts", &[]).unwrap()[0],
            "n",
        ) as usize;
        assert_eq!(
            total,
            acme.feed(None, 0, 200, true).unwrap().len()
                + globex.feed(None, 0, 200, true).unwrap().len()
        );
    }

    #[test]
    fn replicas_of_two_orgs_pin_keys_separately() {
        let db = TempDb::new("replica");
        let acme = db.org("acme", Mode::Replica);
        let globex = db.org("globex", Mode::Replica);

        // Each org's relay accepted a post from its own "dave".
        let source = |org: &str, gseq_pad: usize| {
            let mut up = Authority::new(Sqlite::memory().unwrap(), Mode::Authority, now).unwrap();
            up.org = org.into();
            up.ensure_board("smoko", "").unwrap();
            let dave = person("dave");
            for i in 0..gseq_pad {
                up.accept(envelope(
                    &dave,
                    "cli",
                    "smoko",
                    Kind::Note,
                    &format!("{org} pad {i}"),
                    &ulid(now()),
                    None,
                ))
                .unwrap();
            }
            up.accept(envelope(
                &dave,
                "cli",
                "smoko",
                Kind::Note,
                &format!("{org} hello"),
                &ulid(now()),
                None,
            ))
            .unwrap()
        };
        let from_acme = source("acme", 0);
        let from_globex = source("globex", 1);

        assert_eq!(acme.ingest(&from_acme).unwrap(), Ingest::Stored);
        assert_eq!(acme.ingest(&from_acme).unwrap(), Ingest::Duplicate);
        // Globex's dave has a different key. Acme's pin must not reject him.
        assert_eq!(globex.ingest(&from_globex).unwrap(), Ingest::Stored);
        assert_eq!(
            bodies(&acme.feed(None, 0, 10, true).unwrap()),
            ["acme hello"]
        );
        assert_eq!(
            bodies(&globex.feed(None, 0, 10, true).unwrap()),
            ["globex hello"]
        );
        assert_ne!(
            acme.identity("dave").unwrap().unwrap().key,
            globex.identity("dave").unwrap().unwrap().key
        );
        // Within an org the pin still holds.
        assert_eq!(acme.ingest(&from_globex).unwrap(), Ingest::Rejected);
    }
}
