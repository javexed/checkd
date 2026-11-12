GET statehist
Columns: host_name service_description state
Filter: time >= {now-120}
Filter: time < {now}
AuthUser: bob
OutputFormat: json
ResponseHeader: fixed16
