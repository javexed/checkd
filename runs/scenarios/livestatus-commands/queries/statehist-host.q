GET statehist
Columns: host_name service_description state from until duration duration_ok duration_warning duration_critical duration_unknown duration_unmonitored
Filter: host_name = db
Filter: service_description =
Filter: time >= {now-120}
Filter: time < {now}
OutputFormat: json
ResponseHeader: fixed16
