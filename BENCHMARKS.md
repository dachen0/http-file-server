# Benchmark Report

Tool: ApacheBench 2.3
Workload: 10,000 requests, concurrency 100, 50 KB static file
TLS: disabled for all three

## Summary

| Server                  | Req/sec    | Mean latency | p50  | p99  | Transfer rate      |
|-------------------------|------------|--------------|------|------|--------------------|
| nginx (no sendfile)     | 19,489.64  | 5.131 ms     | 4 ms | 12ms | 956,305 KB/s       |
| nginx (sendfile)        | 21,188.37  | 4.720 ms     | 4 ms | 11ms | 1,039,657 KB/s     |
| **fast-http (sendfile)**| **22,436.11** | **4.457 ms** | **4 ms** | **8 ms** | **1,098,274 KB/s** |

fast-http is **15.1%** faster than nginx without sendfile and **5.9%** faster than nginx with sendfile.

---

## nginx — sendfile off

```
Server Software:   nginx/1.18.0
Document Length:   50000 bytes
Concurrency Level: 100
Time taken:        0.513 seconds
Complete requests: 10000
Failed requests:   0
Total transferred: 502,450,000 bytes

Requests per second: 19489.64 [#/sec] (mean)
Time per request:    5.131 ms (mean)
Transfer rate:       956,305.83 KB/s

Connection Times (ms)
              min  mean[+/-sd] median   max
Connect:        0    1   0.4      1       5
Processing:     1    4   1.2      4      17
Waiting:        0    1   0.6      1      10
Total:          2    5   1.5      4      19

Percentiles:  50%=4  66%=5  75%=6  80%=6  90%=6  95%=6  98%=11  99%=12
```

---

## nginx — sendfile on

```
Server Software:   nginx/1.18.0
Document Length:   50000 bytes
Concurrency Level: 100
Time taken:        0.472 seconds
Complete requests: 10000
Failed requests:   0
Total transferred: 502,450,000 bytes

Requests per second: 21188.37 [#/sec] (mean)
Time per request:    4.720 ms (mean)
Transfer rate:       1,039,657.91 KB/s

Connection Times (ms)
              min  mean[+/-sd] median   max
Connect:        0    1   0.4      1       4
Processing:     0    4   1.0      4      13
Waiting:        0    1   0.6      1       6
Total:          1    5   1.2      4      14

Percentiles:  50%=4  66%=4  75%=4  80%=5  90%=6  95%=7  98%=9  99%=11  100%=14
```

---

## fast-http — sendfile on

```
Server Software:   fast-http/0.1.0
Document Length:   50000 bytes
Concurrency Level: 100
Time taken:        0.446 seconds
Complete requests: 10000
Failed requests:   0
Total transferred: 501,260,000 bytes

Requests per second: 22436.11 [#/sec] (mean)
Time per request:    4.457 ms (mean)
Transfer rate:       1,098,274.03 KB/s

Connection Times (ms)
              min  mean[+/-sd] median   max
Connect:        0    1   0.3      1       3
Processing:     1    4   0.8      3       8
Waiting:        0    2   0.6      1       5
Total:          2    4   0.8      4       9

Percentiles:  50%=4  66%=4  75%=4  80%=5  90%=5  95%=6  98%=7  99%=8  100%=9
```

---

## Notes

- fast-http has a lower p99 tail (8 ms vs 11–12 ms) and a tighter standard deviation on total time (±0.8 ms vs ±1.2–1.5 ms for nginx), suggesting more consistent latency under load.
- nginx with sendfile enabled closes most of the gap with fast-http on throughput, confirming that `sendfile(2)` accounts for the bulk of the difference against nginx's default configuration.
- fast-http runs a single-threaded busy-poll loop with no worker pool; the nginx workers here used the default configuration. The throughput advantage will shrink at higher concurrency levels where multi-threading matters more.
