GET log
Columns: class type host_name service_description contact_name state
Filter: time >= {now-120}
AuthUser: ann
OutputFormat: json
ResponseHeader: fixed16
