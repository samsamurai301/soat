# Operating SOAT

What to watch on a running SOAT server: logs, metrics and health. The code
is in `crates/soat/src/ops.rs`.

## Logs

The server logs one line per request, with its method, route, path, status,
latency and request id. Every response carries the id in `x-request-id`; an
`x-request-id` sent by a proxy in front is kept, so one id follows a request
through both logs.

When standard output is not a terminal (a container, systemd), the lines
are JSON, one object per line, ready for Loki, Elasticsearch or any log
pipeline. On a terminal they are readable text.

| Setting | Effect |
|---|---|
| `RUST_LOG` | What is logged, e.g. `info` (default), `warn`, `zapline::request=warn,info` |
| `ZAPLINE_LOG_FORMAT` | `json`, `compact`, `pretty` or `full`, whatever the output is |

Health probes and metric scrapes (`/health*`, `/metrics`) are not logged.

## Metrics

Set `SOAT_METRICS_TOKEN` to a long random value and point Prometheus at
`/metrics` with it as a bearer token. Without the setting, `/metrics`
answers 404; with it, a missing or wrong token answers 401.

```yaml
scrape_configs:
  - job_name: soat
    authorization:
      credentials: <SOAT_METRICS_TOKEN>
    static_configs:
      - targets: ["shop.example.com:8080"]
```

| Series | What |
|---|---|
| `http_requests_total{method,route,status}` | Requests, by route template (`/api/products/{id}`), never by raw path |
| `http_request_duration_seconds{method,route}` | Latency histogram |
| `http_requests_in_flight` | Requests being served |
| `process_*` | CPU, memory, open files, start time |
| `scheduled_task_*{task}` | Background tasks: event dispatch (`soat-events`), webhooks, retention, ... |
| `soat_queue_waiting{queue}` | Items due now and not done |
| `soat_queue_oldest_seconds{queue}` | How long the oldest waiting item has been due |
| `soat_queue_failed_last_day{queue}` | Items given up after their last retry in the past 24 hours |

The queues are `events` (new events not yet handed to their subscribers),
`subscribers` (deliveries to modules reacting to events: emails, stock,
search) and `webhooks` (deliveries to shops' webhook endpoints). They are
summed over all shops; no series is labelled by shop, so the number of
series stays fixed however many shops a server holds.

Alerts worth having:

```yaml
- alert: SoatQueueStuck
  expr: soat_queue_oldest_seconds > 600
  for: 5m
- alert: SoatDeliveriesGivenUp
  expr: increase(soat_queue_failed_last_day{queue="subscribers"}[1h]) > 0
- alert: SoatErrors
  expr: sum(rate(http_requests_total{status=~"5.."}[5m])) > 0.1
```

## Health

| Endpoint | Answers |
|---|---|
| `GET /health/live` | Always 200 while the process runs: the liveness probe |
| `GET /health/ready` | 200 when the database answers, 503 otherwise: the readiness probe |
| `GET /health` | The same, with each check's status and the uptime |

Checks are `database` (critical), `events` and `webhooks`. A queue whose
oldest due item has waited more than 10 minutes is reported `down` and the
whole report `degraded`, but the server stays ready: a stuck queue is a
reason to look, not to take the shop off the load balancer. The reasons are
logged, not shown on the public endpoint.
