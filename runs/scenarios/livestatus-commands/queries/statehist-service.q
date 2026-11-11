GET statehist
Columns: host_name service_description state from until duration
Filter: host_name = web
Filter: service_description = http
Filter: time >= {now-120}
Filter: time < {now}
OutputFormat: json
ResponseHeader: fixed16
