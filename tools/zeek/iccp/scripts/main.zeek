##! ICCP / TASE.2 over MMS: mms.log (one row per MMS PDU) and iccp.log (one row per TASE.2 object operation).
##! The Spicy analyzer in ../analyzer raises ICCP::pdu; everything below is bookkeeping and TASE.2 naming.
##! The TASE.2 names and encodings are as remembered from IEC 60870-6-503/802, not checked against a licensed copy
##! (README.md, "Assumptions"), so they are kept in the two tables near the top of this file.

module ICCP;

# A GetNameList or a report of a real EMS can list thousands of names: the default of 500 elements per log field container made Zeek
# raise log_container_field_truncated (a weird.log row) on the chaugan capture (conformance/iccp).
redef Log::default_max_field_container_elements = 10000;
# ... and the same for a long string (a structure value in iccp.log): log_string_field_truncated
redef Log::default_max_field_string_bytes = 100000;
redef Log::default_max_total_container_elements = 100000;
redef Log::default_max_total_string_bytes = 1000000;

export {
	redef enum Log::ID += { MMS_LOG, LOG };

	type MmsInfo: record {
		ts:        time     &log;
		uid:       string   &log;
		id:        conn_id  &log;
		is_orig:   bool     &log;
		pdu:       string   &log;
		service:   string   &log &optional;
		invoke_id: int      &log &optional;
		scope:     string   &log &optional;
		domain:    string   &log &optional;
		target:    string   &log &optional;
		items:     vector of string &log &optional;
		values:    vector of string &log &optional;
		result:    string   &log &optional;
		error:     string   &log &optional;
		rtt:       interval &log &optional;
		detail:    string   &log &optional;
	};

	type Info: record {
		ts:        time   &log;
		uid:       string &log;
		id:        conn_id &log;
		invoke_id: int    &log &optional;
		## read, write, report, define_dataset, start_transfer_set
		op:        string &log;
		scope:     string &log &optional;
		domain:    string &log &optional;
		name:      string &log;
		## TASE.2 object class of the name (bilateral_table_id, tase2_version, transfer_set, infomsg, data ...).
		class:     string &log;
		## Shape of the value: real, realq, discrete, discreteq, state, stateq, bitstring, struct ...
		vtype:     string &log &optional;
		value:     string &log &optional;
		quality:   string &log &optional;
		result:    string &log &optional;
		transfer_set:  string &log &optional;
		ds_conditions: set[string] &log &optional;
		timetag:   string &log &optional;
		bilateral_table_id: string &log &optional;
		tase2_version:      string &log &optional;
		supported_features: string &log &optional;
		## Block names of the set bits in supported_features, e.g. BasicServices, ExtendedConditions.
		supported_blocks:   vector of string &log &optional;
	};

	global log_mms: event(rec: MmsInfo);
	global log_iccp: event(rec: Info);
}

# Names that exist only in TASE.2. Seeing one marks the connection as TASE.2 (iccp.log stays empty on plain MMS).
const special_names: set[string] = {
	"Bilateral_Table_ID", "TASE2_Version", "Supported_Features", "Next_DSTransfer_Set",
	"Transfer_Set_Name", "Transfer_Set_Time_Stamp", "DSConditions_Detected", "Event_Code_Detected",
} &redef;

const bit_classes: set[string] = { "supported_features", "dsconditions_detected" };

# DSConditions_Detected bit string, bit 0 first.
const ds_condition_bits: vector of string = {
	"Interval", "Integrity", "ObjectChange", "OperatorRequest", "OtherExternalEvent",
} &redef;

# Supported_Features bit string, bit n is conformance block n+1 (IEC 60870-6-503; docs/field-device-iccp.md, ww-otj.62).
# Set bits past the named ones show as "Bit<n>".
const supported_block_names: vector of string = {
	"BasicServices", "ExtendedConditions", "BlockedTransfers", "InformationMessages", "DeviceControl",
	"Programs", "Events", "AdditionalEvents", "TimeSeries",
} &redef;

# The first three values of a transfer report against a named variable list carry no names on the wire.
const report_header: vector of string = { "Transfer_Set_Name", "Transfer_Set_Time_Stamp", "DSConditions_Detected" };

type Req: record {
	ts:      time;
	service: string;
	names:   vector of string;
	scope:   string;
	domain:  string;
	target:  string;
	vtypes:  vector of string;
	values:  vector of string;
	quals:   vector of string;
};

type State: record {
	reqs:       table[int] of Req;
	is_tase2:   bool &default=F;
	bt:         string &optional;
	version:    string &optional;
	features:   string &optional;
	tsets:      set[string];
	datasets:   set[string];
};

redef record connection += { iccp: State &optional; };

event zeek_init() &priority=5
	{
	Log::create_stream(ICCP::MMS_LOG, [$columns=MmsInfo, $ev=log_mms, $path="mms"]);
	Log::create_stream(ICCP::LOG, [$columns=Info, $ev=log_iccp, $path="iccp"]);
	}

function state(c: connection): State
	{
	if ( ! c?$iccp )
		c$iccp = State();
	return c$iccp;
	}

function bare(item: string): string
	{
	local p = split_string1(item, /\//);
	return |p| == 2 ? p[1] : item;
	}

function name_class(st: State, item: string): string
	{
	local n = bare(item);
	if ( n in special_names )
		return to_lower(n);
	if ( n in st$tsets )
		return "transfer_set";
	if ( item in st$datasets )
		return "dataset";
	if ( /^(IM_|Info)/ in n )
		return "infomsg";
	return st$is_tase2 && /\// in item ? "data" : "";
	}

# In bit order (a vector: a set would log in a different order from run to run).
function bit_names(bitstr: string, names: vector of string): vector of string
	{
	local r: vector of string;
	local i = 0;
	while ( i < |bitstr| )
		{
		if ( sub_bytes(bitstr, i + 1, 1) == "1" )
			r += i < |names| ? names[i] : fmt("Bit%d", i + 1);
		++i;
		}
	return r;
	}

function conditions(bitstr: string): set[string]
	{
	local r: set[string];
	for ( i in ds_condition_bits )
		if ( i < |bitstr| && sub_bytes(bitstr, i + 1, 1) == "1" )
			add r[ds_condition_bits[i]];
	return r;
	}

# "{ICC1,TSet1}" -> "TSet1" (Transfer_Set_Name is a structure whose last member is the name).
function last_member(v: string): string
	{
	local parts = split_string(gsub(v, /[{}]/, ""), /;/);
	return |parts| > 0 ? parts[|parts| - 1] : "";
	}

function row(c: connection, st: State, inv: int, op: string, scope: string, domain: string, name: string,
             vtype: string, value: string, quality: string, result: string): Info
	{
	local r = Info($ts=network_time(), $uid=c$uid, $id=c$id, $op=op, $name=name, $class=name_class(st, name));
	if ( inv >= 0 ) r$invoke_id = inv;
	if ( scope != "" ) r$scope = scope;
	if ( domain != "" ) r$domain = domain;
	if ( /\// in name )
		{
		local d = split_string1(name, /\//);
		r$scope = "domain";
		r$domain = d[0];
		}
	# Supported_Features and friends are plain bit strings, whatever their length: undo the stateq guess.
	if ( r$class in bit_classes && vtype == "stateq" )
		{
		vtype = "bitstring";
		quality = "";
		}
	if ( vtype != "" ) r$vtype = vtype;
	if ( value != "" ) r$value = value;
	if ( quality != "" ) r$quality = quality;
	if ( result != "" ) r$result = result;
	if ( st?$bt ) r$bilateral_table_id = st$bt;
	if ( st?$version ) r$tase2_version = st$version;
	if ( st?$features )
		{
		r$supported_features = st$features;
		r$supported_blocks = bit_names(st$features, supported_block_names);
		}
	return r;
	}

# What a read response or a write teaches about the peer.
function learn(st: State, name: string, value: string)
	{
	local n = bare(name);
	if ( n == "Bilateral_Table_ID" ) st$bt = value;
	else if ( n == "TASE2_Version" ) st$version = value;
	else if ( n == "Supported_Features" ) st$features = value;
	else if ( n == "Next_DSTransfer_Set" ) add st$tsets[last_member(value)];
	}

function mark_tase2(st: State, names: vector of string, target: string)
	{
	for ( i in names )
		if ( bare(names[i]) in special_names )
			st$is_tase2 = T;
	if ( bare(target) in special_names )
		st$is_tase2 = T;
	}

function summary(vtypes: vector of string, values: vector of string, quals: vector of string): vector of string
	{
	local r: vector of string;
	for ( i in values )
		r += vtypes[i] + "=" + values[i] + (quals[i] != "" ? "[" + quals[i] + "]" : "");
	return r;
	}

event ICCP::pdu(c: connection, is_orig: bool, kind: string, service: string, invoke_id: int, scope: string,
                domain: string, target: string, detail: string, err: string, names: vector of string,
                vtypes: vector of string, values: vector of string, quals: vector of string)
	{
	local st = state(c);
	local m = MmsInfo($ts=network_time(), $uid=c$uid, $id=c$id, $is_orig=is_orig, $pdu=kind);
	if ( service != "" ) m$service = service;
	if ( invoke_id >= 0 ) m$invoke_id = invoke_id;
	if ( scope != "" ) m$scope = scope;
	if ( domain != "" ) m$domain = domain;
	if ( target != "" ) m$target = target;
	if ( detail != "" ) m$detail = detail;
	if ( err != "" ) m$error = err;
	local rows: vector of Info;

	if ( kind == "confirmed_req" )
		{
		mark_tase2(st, names, target);
		# ponytail: an unanswered request stays in the table until the connection goes; a bounded table if that matters.
		if ( invoke_id >= 0 )
			st$reqs[invoke_id] = Req($ts=network_time(), $service=service, $names=names, $scope=scope,
			                         $domain=domain, $target=target, $vtypes=vtypes, $values=values, $quals=quals);
		m$items = names;
		if ( |values| > 0 ) m$values = summary(vtypes, values, quals);
		if ( service == "defineNamedVariableList" && st$is_tase2 ) add st$datasets[target];
		}
	else if ( kind == "confirmed_resp" || kind == "confirmed_err" )
		{
		local req: Req;
		local have = invoke_id in st$reqs;
		if ( have )
			{
			req = st$reqs[invoke_id];
			delete st$reqs[invoke_id];
			m$rtt = network_time() - req$ts;
			if ( m?$service == F || m$service == "" ) m$service = req$service;
			m$items = req$names;
			if ( req$scope != "" ) m$scope = req$scope;
			if ( req$domain != "" ) m$domain = req$domain;
			if ( req$target != "" ) m$target = req$target;
			}
		m$result = kind == "confirmed_err" ? "error" : "success";
		if ( ! have || |names| > 0 )
			m$items = names;
		if ( |values| > 0 ) m$values = summary(vtypes, values, quals);

		if ( have )
			{
			local inv = invoke_id;
			if ( req$service == "read" )
				{
				for ( i in values )
					{
					local nm = i < |req$names| ? req$names[i] : "";
					# a refused read teaches nothing (its value is the error code)
					if ( kind == "confirmed_resp" && vtypes[i] != "error" )
						learn(st, nm, values[i]);
					rows += row(c, st, inv, "read", req$scope, req$domain, nm, vtypes[i], values[i], quals[i],
					            vtypes[i] == "error" ? "failure" : "success");
					}
				if ( kind == "confirmed_err" )
					for ( i in req$names )
						rows += row(c, st, inv, "read", req$scope, req$domain, req$names[i], "", "", "", "error");
				}
			else if ( req$service == "write" )
				{
				for ( i in req$names )
					{
					local res = kind == "confirmed_err" ? "error" : (i < |values| ? values[i] : "");
					local r = row(c, st, inv, "write", req$scope, req$domain, req$names[i],
					              i < |req$vtypes| ? req$vtypes[i] : "", i < |req$values| ? req$values[i] : "",
					              i < |req$quals| ? req$quals[i] : "", res);
					if ( r$class == "transfer_set" )
						r$op = "start_transfer_set";
					rows += r;
					}
				}
			else if ( req$service == "defineNamedVariableList" && st$is_tase2 )
				{
				local members = "";
				for ( i in req$names )
					members += (i > 0 ? "," : "") + req$names[i];
				rows += row(c, st, inv, "define_dataset", req$scope, req$domain, req$target, "", members, "",
				            kind == "confirmed_err" ? "error" : "success");
				}
			}
		}
	else if ( kind == "unconfirmed" && service == "informationReport" )
		{
		mark_tase2(st, names, target);
		local pos = names;
		# transfer report against a named list: name, time stamp and conditions come first, unnamed
		if ( |names| == 0 && target != "" && |values| >= 3 && vtypes[0] == "struct" && vtypes[2] == "bitstring" )
			{
			pos = vector();
			for ( i in report_header )
				pos += report_header[i];
			st$is_tase2 = T;
			}
		m$items = pos;
		if ( target != "" ) m$target = target;
		m$values = summary(vtypes, values, quals);
		local ts_name = "";
		local ts_stamp = "";
		local conds: set[string];
		for ( i in values )
			{
			local nm2 = i < |pos| ? pos[i] : "";
			local b = bare(nm2);
			if ( b == "Transfer_Set_Name" ) ts_name = last_member(values[i]);
			else if ( b == "Transfer_Set_Time_Stamp" ) ts_stamp = values[i];
			else if ( b == "DSConditions_Detected" ) conds = conditions(values[i]);
			}
		for ( i in values )
			{
			local nm3 = i < |pos| ? pos[i] : (target != "" ? fmt("%s[%d]", target, i) : "");
			local r2 = row(c, st, -1, "report", scope, domain, nm3, vtypes[i], values[i], quals[i], "");
			r2$scope = scope;
			if ( target != "" ) r2$transfer_set = target;
			if ( ts_name != "" ) r2$transfer_set = ts_name;
			if ( ts_stamp != "" ) r2$timetag = ts_stamp;
			if ( |conds| > 0 ) r2$ds_conditions = conds;
			if ( st$is_tase2 )
				rows += r2;
			}
		}

	Log::write(ICCP::MMS_LOG, m);
	if ( st$is_tase2 )
		for ( i in rows )
			Log::write(ICCP::LOG, rows[i]);
	}
