//! The built-in templates: what every shop sends until it words its own.

/// A template a shop can send.
pub struct Builtin {
    pub key: &'static str,
    /// What it is for, shown to staff.
    pub description: &'static str,
    pub subject: &'static str,
    pub text: &'static str,
    pub html: &'static str,
}

/// Every template, in the order staff see them.
pub const BUILTINS: &[Builtin] = &[
    Builtin {
        key: "order_confirmation",
        description: "To the buyer when an order is placed.",
        subject: "Order {{order.number}} at {{shop.name}}",
        text: concat!(
            "Thank you for your order!\n\nOrder {{order.number}}\n\n",
            "{{#each order.lines}}{{quantity}} x {{title}}  {{total}}\n{{/each}}\n",
            "Subtotal: {{order.subtotal}}\n",
            "{{#if order.discount_code}}Discount ({{order.discount_code}}): -{{order.discount}}\n{{/if}}",
            "Shipping ({{order.shipping_name}}): {{order.shipping}}\n",
            "Total: {{order.total}}\n",
            "{{#if order.prices_include_tax}}Including tax: {{order.tax}}{{else}}Tax: {{order.tax}}{{/if}}\n\n",
            "{{#if order.shipping_address}}Shipping to:\n{{order.shipping_address.text}}\n\n{{/if}}",
            "{{shop.name}}\n"
        ),
        html: concat!(
            "<p>Thank you for your order!</p><h2>Order {{order.number}}</h2>",
            "<table>{{#each order.lines}}<tr><td>{{quantity}} &times; {{title}}</td><td align=\"right\">{{total}}</td></tr>{{/each}}</table>",
            "<p>Subtotal: {{order.subtotal}}<br>",
            "{{#if order.discount_code}}Discount ({{order.discount_code}}): -{{order.discount}}<br>{{/if}}",
            "Shipping ({{order.shipping_name}}): {{order.shipping}}<br>",
            "<strong>Total: {{order.total}}</strong><br>",
            "{{#if order.prices_include_tax}}Including tax: {{order.tax}}{{else}}Tax: {{order.tax}}{{/if}}</p>",
            "{{#if order.shipping_address}}<p>Shipping to:<br>{{#each order.shipping_address.lines}}{{this}}<br>{{/each}}</p>{{/if}}",
            "<p>{{shop.name}}</p>"
        ),
    },
    Builtin {
        key: "payment_received",
        description: "To the buyer when an order is paid in full (off by default: most payments are made while ordering).",
        subject: "Payment received for order {{order.number}}",
        text: "We have received your payment of {{order.total}} for order {{order.number}}.\n\n{{shop.name}}\n",
        html: "<p>We have received your payment of {{order.total}} for order {{order.number}}.</p><p>{{shop.name}}</p>",
    },
    Builtin {
        key: "shipping_confirmation",
        description: "To the buyer when a shipment leaves.",
        subject: "Order {{order.number}} is on its way",
        text: concat!(
            "Good news: your order {{order.number}} has shipped.\n\n",
            "{{#each fulfillment.lines}}{{quantity}} x {{title}}\n{{/each}}\n",
            "{{#if fulfillment.carrier}}Carrier: {{fulfillment.carrier}}\n{{/if}}",
            "{{#if fulfillment.tracking_number}}Tracking number: {{fulfillment.tracking_number}}\n{{/if}}",
            "{{#if fulfillment.tracking_url}}Track it: {{fulfillment.tracking_url}}\n{{/if}}",
            "\n{{shop.name}}\n"
        ),
        html: concat!(
            "<p>Good news: your order {{order.number}} has shipped.</p>",
            "<ul>{{#each fulfillment.lines}}<li>{{quantity}} &times; {{title}}</li>{{/each}}</ul>",
            "<p>{{#if fulfillment.carrier}}Carrier: {{fulfillment.carrier}}<br>{{/if}}",
            "{{#if fulfillment.tracking_number}}Tracking number: {{fulfillment.tracking_number}}<br>{{/if}}",
            "{{#if fulfillment.tracking_url}}<a href=\"{{fulfillment.tracking_url}}\">Track your parcel</a>{{/if}}</p>",
            "<p>{{shop.name}}</p>"
        ),
    },
    Builtin {
        key: "order_canceled",
        description: "To the buyer when an order is canceled.",
        subject: "Order {{order.number}} has been canceled",
        text: "Your order {{order.number}} has been canceled.{{#if order.cancel_reason}}\nReason: {{order.cancel_reason}}{{/if}}\n\nAnything you paid is refunded.\n\n{{shop.name}}\n",
        html: "<p>Your order {{order.number}} has been canceled.{{#if order.cancel_reason}}<br>Reason: {{order.cancel_reason}}{{/if}}</p><p>Anything you paid is refunded.</p><p>{{shop.name}}</p>",
    },
    Builtin {
        key: "refund_issued",
        description: "To the buyer when money is refunded.",
        subject: "Refund for order {{order.number}}",
        text: "We have refunded {{refund.amount}} for order {{order.number}}.{{#if refund.reason}}\nReason: {{refund.reason}}{{/if}}\nIt can take a few days to reach your account.\n\n{{shop.name}}\n",
        html: "<p>We have refunded {{refund.amount}} for order {{order.number}}.{{#if refund.reason}}<br>Reason: {{refund.reason}}{{/if}}</p><p>It can take a few days to reach your account.</p><p>{{shop.name}}</p>",
    },
    Builtin {
        key: "staff_new_order",
        description: "To the shop's staff address when an order is placed.",
        subject: "New order {{order.number}}: {{order.total}}",
        text: concat!(
            "New order {{order.number}} from {{order.email}}.\n\n",
            "{{#each order.lines}}{{quantity}} x {{title}} ({{sku}})  {{total}}\n{{/each}}\n",
            "Total: {{order.total}}\n"
        ),
        html: concat!(
            "<p>New order {{order.number}} from {{order.email}}.</p>",
            "<table>{{#each order.lines}}<tr><td>{{quantity}} &times; {{title}} ({{sku}})</td><td align=\"right\">{{total}}</td></tr>{{/each}}</table>",
            "<p><strong>Total: {{order.total}}</strong></p>"
        ),
    },
    Builtin {
        key: "password_reset",
        description: "To a customer who asks to reset their password. Not kept once sent.",
        subject: "Reset your password at {{shop.name}}",
        text: concat!(
            "Hello{{#if customer.first_name}} {{customer.first_name}}{{/if}},\n\n",
            "Someone asked to reset the password of your account at {{shop.name}}. ",
            "{{#if access.link}}Set a new one here:\n{{access.link}}\n\n{{/if}}",
            "{{#if access.link}}Or enter{{else}}Enter{{/if}} this code on the shop's password page: {{access.code}}\n\n",
            "It works once, for {{access.minutes}} minutes. If you did not ask, ignore this email.\n\n{{shop.name}}\n"
        ),
        html: concat!(
            "<p>Hello{{#if customer.first_name}} {{customer.first_name}}{{/if}},</p>",
            "<p>Someone asked to reset the password of your account at {{shop.name}}.</p>",
            "{{#if access.link}}<p><a href=\"{{access.link}}\">Set a new password</a></p>{{/if}}",
            "<p>{{#if access.link}}Or enter{{else}}Enter{{/if}} this code on the shop's password page: <code>{{access.code}}</code></p>",
            "<p>It works once, for {{access.minutes}} minutes. If you did not ask, ignore this email.</p><p>{{shop.name}}</p>"
        ),
    },
    Builtin {
        key: "account_activation",
        description: "To a customer who ordered as a guest and asks for an account. Not kept once sent.",
        subject: "Activate your account at {{shop.name}}",
        text: concat!(
            "Hello{{#if customer.first_name}} {{customer.first_name}}{{/if}},\n\n",
            "You have ordered at {{shop.name}} before. ",
            "{{#if access.link}}Choose a password to activate your account:\n{{access.link}}\n\n{{/if}}",
            "{{#if access.link}}Or enter{{else}}Enter{{/if}} this code on the shop's password page: {{access.code}}\n\n",
            "It works once, for {{access.minutes}} minutes. If you did not ask, ignore this email.\n\n{{shop.name}}\n"
        ),
        html: concat!(
            "<p>Hello{{#if customer.first_name}} {{customer.first_name}}{{/if}},</p>",
            "<p>You have ordered at {{shop.name}} before.</p>",
            "{{#if access.link}}<p><a href=\"{{access.link}}\">Choose a password to activate your account</a></p>{{/if}}",
            "<p>{{#if access.link}}Or enter{{else}}Enter{{/if}} this code on the shop's password page: <code>{{access.code}}</code></p>",
            "<p>It works once, for {{access.minutes}} minutes. If you did not ask, ignore this email.</p><p>{{shop.name}}</p>"
        ),
    },    Builtin {
        key: "cart_recovery",
        description: "To a buyer who left a cart with their email, once, when the shop has abandoned cart emails on. Not kept once sent.",
        subject: "You left something in your cart at {{shop.name}}",
        text: concat!(
            "Hello,\n\n",
            "You left these in your cart at {{shop.name}}:\n\n",
            "{{#each cart.lines}}{{quantity}} x {{title}}\n{{/each}}\n",
            "{{#if cart.link}}Pick up where you left off:\n{{cart.link}}\n\n{{/if}}",
            "Prices and stock are checked again when you come back.\n\n{{shop.name}}\n"
        ),
        html: concat!(
            "<p>Hello,</p>",
            "<p>You left these in your cart at {{shop.name}}:</p>",
            "<ul>{{#each cart.lines}}<li>{{quantity}} &times; {{title}}</li>{{/each}}</ul>",
            "{{#if cart.link}}<p><a href=\"{{cart.link}}\">Pick up where you left off</a></p>{{/if}}",
            "<p>Prices and stock are checked again when you come back.</p><p>{{shop.name}}</p>"
        ),
    },
    Builtin {
        key: "gift_card_issued",
        description: "To the buyer of a gift card, with its code, once the order is paid. Not kept once sent.",
        subject: "Your {{gift_card.amount}} gift card from {{shop.name}}",
        text: concat!(
            "Hello,\n\n",
            "Here is the {{gift_card.amount}} gift card you bought at {{shop.name}}",
            "{{#if order.number}} with order {{order.number}}{{/if}}.\n\n",
            "Code: {{gift_card.code}}\n\n",
            "{{#if gift_card.expires_at}}It can be used until {{gift_card.expires_at}}. {{/if}}",
            "Enter the code at checkout. Keep it safe: anyone with the code can spend it.\n\n{{shop.name}}\n"
        ),
        html: concat!(
            "<p>Hello,</p>",
            "<p>Here is the {{gift_card.amount}} gift card you bought at {{shop.name}}",
            "{{#if order.number}} with order {{order.number}}{{/if}}.</p>",
            "<p>Code: <code>{{gift_card.code}}</code></p>",
            "<p>{{#if gift_card.expires_at}}It can be used until {{gift_card.expires_at}}. {{/if}}",
            "Enter the code at checkout. Keep it safe: anyone with the code can spend it.</p><p>{{shop.name}}</p>"
        ),
    },
];

/// The built-in template `key`.
pub fn builtin(key: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.key == key)
}

#[cfg(test)]
mod tests {
    use soat_core::template::Template;

    use super::BUILTINS;

    #[test]
    fn builtins_parse() {
        for b in BUILTINS {
            for t in [b.subject, b.text, b.html] {
                Template::parse(t).unwrap_or_else(|e| panic!("{}: {e}", b.key));
            }
        }
    }
}
