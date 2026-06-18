# Customer Notice: License Enforcement

## Markdown Email

Subject: Action required: AILinkMax license checks will be enforced on YYYY-MM-DD

Hi {{customer_name}},

As part of the upcoming AILinkMax 1.0 launch, AILinkMax will begin enforcing the
license check that has been running in warning-only mode since {{warn_start}}.
After {{enforce_date}}, requests on instances without a valid license will
receive `HTTP 402 Payment Required`.

Your instance status is currently: {{license_state}}.

To avoid disruption, please {{required_action}} by {{deadline}}.

If you need help, reply to this email or contact {{support_contact}}.

Thanks,
AILinkMax Support

## Plain Text Variant

Subject: Action required: AILinkMax license checks will be enforced on YYYY-MM-DD

Hi {{customer_name}},

AILinkMax will begin enforcing license checks on {{enforce_date}}. Instances
without a valid license will receive HTTP 402 Payment Required after that date.

Your current license status: {{license_state}}
Required action: {{required_action}}
Deadline: {{deadline}}

Questions: {{support_contact}}

AILinkMax Support

## Send Checklist

- Confirm `license_enforce=warn` has been deployed before the first notice.
- Fill `{{warn_start}}`, `{{enforce_date}}`, `{{license_state}}`,
  `{{required_action}}`, `{{deadline}}`, and `{{support_contact}}`.
- Attach affected-customer export ID to the send record.
- Record sent timestamps in the rollout tracker.
