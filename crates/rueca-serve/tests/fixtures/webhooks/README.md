Minimal webhook payloads written by hand from the hosts' documented shapes
(RFC 0008 §7.3 sources): only the fields Rueca reads, plus a few neighbours
for realism. Not recorded deliveries.

- GitHub: https://docs.github.com/en/webhooks/webhook-events-and-payloads
  (`push`, `create`, `delete`, `pull_request`, `ping`).
- GitLab: https://docs.gitlab.com/user/project/integrations/webhook_events/
  (Push, Tag Push, Merge Request hooks).
- Bitbucket Cloud: https://support.atlassian.com/bitbucket-cloud/docs/event-payloads/
  (`repo:push`, `pullrequest:created`, `pullrequest:fulfilled`).

`REPO_URL` is replaced by the test's repository URL before signing.
