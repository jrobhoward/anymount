#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use proptest::prelude::*;

use super::*;

#[test]
fn trait_offset____resume_after_zero_or_dot____is_the_first_entry() {
    assert_eq!(trait_offset(0), 0);
    assert_eq!(trait_offset(DOT), 0);
    assert_eq!(trait_offset(DOTDOT), 0);
}

#[test]
fn for_entry____first_trait_offset____is_never_a_dot_or_dotdot_cookie() {
    assert!(for_entry(0) > DOTDOT);
}

proptest! {
    /// A resume request's cookie is the *last-served* entry's own cookie —
    /// [`trait_offset`] must therefore land one past it (the entry itself
    /// must not be re-served), not back at the same offset.
    #[test]
    fn for_entry____any_offset____trait_offset_of_its_cookie_resumes_one_past_it(offset in 0u64..1_000_000) {
        prop_assert_eq!(trait_offset(for_entry(offset)), offset + 1);
    }

    #[test]
    fn for_entry____distinct_offsets____never_collide(a in 0u64..10_000, b in 0u64..10_000) {
        prop_assume!(a != b);
        prop_assert_ne!(for_entry(a), for_entry(b));
    }

    #[test]
    fn for_entry____any_offset____cookie_exceeds_dot_and_dotdot(offset in 0u64..1_000_000) {
        prop_assert!(for_entry(offset) > DOTDOT);
    }
}
