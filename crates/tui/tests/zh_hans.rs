//! The TUI in Simplified Chinese: its own process, so the locale can be fixed
//! before any text is shown. Key screens are rendered at a narrow and a wide
//! size; Chinese text must show, and nothing may be wider than the terminal.

use std::path::PathBuf;

use axel_tui::launcher::Launcher;
use axel_tui::model::{AgentColor, AgentRow, Harness, ItemBody, ItemKind, ProjectRow, RuntimeMode};
use axel_tui::thread::{ThreadView, fixtures};
use axel_tui::util::text_width;
use axel_tui::view::{launcher_view, thread_view};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const WIDTHS: [u16; 2] = [60, 100];
const HEIGHT: u16 = 30;

fn chinese() {
    assert!(axel_i18n::init(axel_i18n::Locale::ZhHans), "the locale is fixed before any text is shown");
}

/// The screen, a string per row; the cell a wide character covers after its
/// own is skipped, so each row reads as a terminal shows it.
fn rows(term: &Terminal<TestBackend>) -> Vec<String> {
    let buf = term.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            let mut row = String::new();
            let mut x = 0;
            while x < buf.area.width {
                let symbol = buf[(x, y)].symbol();
                row.push_str(symbol);
                x += u16::try_from(text_width(symbol).max(1)).expect("a cell is one or two wide");
            }
            row
        })
        .collect()
}

fn assert_fits(rows: &[String], width: u16, screen: &str) {
    for (y, row) in rows.iter().enumerate() {
        assert!(text_width(row) <= usize::from(width), "{screen} at {width}: row {y} is {} cells: {row:?}", text_width(row));
    }
}

/// A thread whose pending question and your message are in Chinese, so
/// wrapping meets wide characters with no spaces to break at.
fn thread() -> ThreadView {
    let mut data = fixtures::data();
    data.title = "修复登录页面的错误".into();
    if let ItemBody::Text(t) = &mut data.items[0].body {
        *t = "请帮我修复登录页面在刷新令牌之后跳回首页的错误，并补上测试".into();
    }
    let question = data.items.iter_mut().find(|it| it.kind == ItemKind::Permission).expect("the fixture has a pending permission");
    if let ItemBody::Choice { prompt, .. } = &mut question.body {
        *prompt = "运行 cargo test 来验证登录流程在令牌过期后仍然正常".into();
    }
    ThreadView::new("T1", data)
}

fn agent(name: &str) -> AgentRow {
    AgentRow {
        pubkey: format!("PK_{name}"),
        name: name.into(),
        harness: Harness::Claude,
        model: Some("sonnet".into()),
        color: Some(AgentColor::Purple),
        runtime_mode: RuntimeMode::Autopilot,
        last_seen_at: Some(1_000),
        unread: 3,
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn the_tui_speaks_simplified_chinese_within_the_width() {
    chinese();
    thread_view_with_a_pending_question();
    composer_footer();
    launcher();
}

fn thread_view_with_a_pending_question() {
    for width in WIDTHS {
        let v = thread();
        // Every row the timeline draws, measured before the frame clips it.
        let body = usize::from(width) - 3;
        for row in v.rows() {
            for line in thread_view::row_lines(&v, &row, body, false) {
                assert!(line.width() <= body, "timeline row at {width}: {} cells: {line:?}", line.width());
            }
        }
        let mut term = Terminal::new(TestBackend::new(width, HEIGHT)).unwrap();
        term.draw(|f| thread_view::render(f, &v, 5_000)).unwrap();
        let screen = rows(&term);
        let all = screen.concat();
        assert_fits(&screen, width, "thread view");
        assert!(all.contains("Atlas 请求权限："), "the question, in Chinese, at {width}:\n{}", screen.join("\n"));
        assert!(all.contains("仍然正常"), "the whole question shows, wrapped, at {width}:\n{}", screen.join("\n"));
        assert!(all.contains("约 $0.42"), "the header's cost at {width}:\n{}", screen.join("\n"));
        assert!(all.contains("跳回首页"), "your message, wrapped, at {width}:\n{}", screen.join("\n"));
    }
}

fn composer_footer() {
    for width in WIDTHS {
        let mut v = thread();
        v.compose_only = true;
        v.composer.paste("请先看一下认证模块，再改登录页面");
        let mut term = Terminal::new(TestBackend::new(width, 12)).unwrap();
        term.draw(|f| thread_view::render(f, &v, 5_000)).unwrap();
        let screen = rows(&term);
        assert_fits(&screen, width, "composer");
        let footer = screen.last().expect("the composer has a footer");
        assert!(footer.contains("自动模式已开启"), "the mode, in Chinese, at {width}: {footer:?}");
        assert!(screen.iter().any(|r| r.contains("请先看一下认证模块")), "what was typed at {width}:\n{}", screen.join("\n"));
        if width >= 100 {
            assert!(footer.contains("发送"), "the key help, in Chinese, at {width}: {footer:?}");
        }
    }
}

fn launcher() {
    for width in WIDTHS {
        let mut l = Launcher::new(
            PathBuf::from("/work/axel-next"),
            vec![ProjectRow { name: "pay-kit".into(), path: PathBuf::from("/work/pay-kit") }],
            vec![agent("atlas"), agent("nova")],
            vec!["claude".into(), "codex".into()],
        );
        let mut term = Terminal::new(TestBackend::new(width, HEIGHT)).unwrap();
        term.draw(|f| launcher_view::render(f, &l, 5_000)).unwrap();
        let screen = rows(&term);
        assert_fits(&screen, width, "launcher prompt");
        assert!(screen.concat().contains("要做什么？"), "the prompt's question at {width}:\n{}", screen.join("\n"));

        for c in "修复登录页面的错误".chars() {
            l.handle_key(key(KeyCode::Char(c)));
        }
        l.handle_key(key(KeyCode::Tab));
        term.draw(|f| launcher_view::render(f, &l, 5_000)).unwrap();
        let screen = rows(&term);
        assert_fits(&screen, width, "launcher where");
        let all = screen.concat();
        assert!(all.contains("在哪里？") && all.contains("其他路径"), "where, in Chinese, at {width}:\n{}", screen.join("\n"));
        assert!(all.contains("修复登录页面的错误"), "the breadcrumb at {width}:\n{}", screen.join("\n"));

        l.handle_key(key(KeyCode::Enter));
        term.draw(|f| launcher_view::render(f, &l, 5_000)).unwrap();
        let screen = rows(&term);
        assert_fits(&screen, width, "launcher who");
        assert!(screen.concat().contains("+ 新建智能体"), "who, in Chinese, at {width}:\n{}", screen.join("\n"));
    }
}
