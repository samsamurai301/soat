# Accessibility

The admin and the default theme aim at WCAG 2.1 AA, which the European
Accessibility Act requires of online shops from June 2025.

## What the tests check

`crates/soat/tests/accessibility.rs` sets up a shop with sample products,
a collection, a page, a blog post, the cookie banner, a cart and a signed-in
customer. It then crawls every page it can reach by links:

- the admin, as staff;
- the storefront, as a visitor;
- the account pages, as a customer;
- the cart and checkout.

Each page goes through `soat_testing::a11y::check`, and the test fails with
a list of issues by rule and page. The checks:

| WCAG | Rule | Fails on |
|---|---|---|
| 1.1.1 | `image-alt` | an `<img>` (or image button) without `alt` (`alt=""` marks decoration) |
| 1.3.1 | `label` | a field with no `<label>`, `aria-label`, `aria-labelledby` or `title` |
| 1.3.1 | `heading-order` | a heading that skips a level (`<h1>` then `<h3>`) |
| 1.4.4 | `viewport-zoom` | a viewport that stops zooming |
| 2.4.2 | `page-title` | a page without a `<title>` |
| 2.4.3 | `tabindex` | a positive `tabindex` |
| 2.4.4 | `link-name` | a link with no text or name |
| 3.1.1 | `html-lang` | `<html>` without `lang` |
| 4.1.1 | `duplicate-id` | an `id` used twice |
| 4.1.2 | `button-name`, `frame-title` | a button with no text or name, an `<iframe>` without `title` |

It runs in `cargo test`, locally and in CI, so a new page or template is
checked as soon as a link leads to it.

## What it does not check

Some things need a browser or a person, and are not tested:

- colour contrast;
- focus order and visible focus;
- motion;
- how a screen reader reads the page.

Test those by hand when a page's look or behavior changes. A theme
someone else writes is checked by the same rules only if it is the one
installed in the test.

To hide text visually but keep it for screen readers, use the theme's
`visually-hidden` class.
