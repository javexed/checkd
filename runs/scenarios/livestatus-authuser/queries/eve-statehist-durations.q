GET statehist
Columns: host_name state duration duration_ok duration_warning duration_critical duration_unknown duration_unmonitored
Filter: host_name = pas
Filter: time >= {now-120}
Filter: time < {now}
AuthUser: eve
OutputFormat: json
ResponseHeader: fixed16
