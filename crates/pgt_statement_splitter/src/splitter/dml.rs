use pgt_lexer::SyntaxKind;

use super::{
    Splitter,
    common::{parenthesis, unknown},
};

pub(crate) fn cte(p: &mut Splitter) {
    p.expect(SyntaxKind::WITH_KW);

    loop {
        p.expect(SyntaxKind::IDENT);
        p.expect(SyntaxKind::AS_KW);
        parenthesis(p);

        if p.current() == SyntaxKind::COMMA {
            p.advance();
        } else {
            break;
        }
    }

    unknown(
        p,
        &[
            SyntaxKind::SELECT_KW,
            SyntaxKind::INSERT_KW,
            SyntaxKind::UPDATE_KW,
            SyntaxKind::DELETE_KW,
            SyntaxKind::MERGE_KW,
        ],
    );
}

pub(crate) fn select(p: &mut Splitter) {
    p.expect(SyntaxKind::SELECT_KW);

    unknown(p, &[]);
}

pub(crate) fn insert(p: &mut Splitter) {
    p.expect(SyntaxKind::INSERT_KW);
    p.expect(SyntaxKind::INTO_KW);

    unknown(p, &[SyntaxKind::SELECT_KW]);
}

pub(crate) fn update(p: &mut Splitter) {
    p.expect(SyntaxKind::UPDATE_KW);

    unknown(p, &[]);
}

pub(crate) fn delete(p: &mut Splitter) {
    p.expect(SyntaxKind::DELETE_KW);
    p.expect(SyntaxKind::FROM_KW);

    unknown(p, &[]);
}
