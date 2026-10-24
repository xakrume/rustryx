//! Rules for the wave 2 foundation sections: `tasks`, `outbox`, `downloads` and `egress`.

use ipnet::IpNet;

use super::Report;
use super::network::{is_plain_host, private_cidr};
use crate::AppConfig;
use crate::schema::{EgressPolicyConfig, EgressSmtpConfig};

pub(super) fn foundation(config: &AppConfig, report: &mut Report) {
    tasks(config, report);
    outbox(config, report);
    downloads(config, report);
    egress(config, report);
}

fn tasks(config: &AppConfig, report: &mut Report) {
    let t = &config.tasks;
    report.range(
        "tasks.max_concurrent_runs",
        u64::from(t.max_concurrent_runs),
        1,
        64,
    );
    report.range("tasks.scheduler_tick_secs", t.scheduler_tick_secs, 1, 300);
    report.range(
        "tasks.run_retention_days",
        u64::from(t.run_retention_days),
        1,
        3650,
    );
    report.range(
        "tasks.log_retention_days",
        u64::from(t.log_retention_days),
        1,
        3650,
    );
    // The log of a run is deleted together with the run (cascade), so a longer log
    // retention would promise something the schema cannot keep.
    if t.log_retention_days > t.run_retention_days {
        report.err(
            "tasks.log_retention_days",
            "must not exceed tasks.run_retention_days: a log is deleted with its run",
        );
    }
}

fn outbox(config: &AppConfig, report: &mut Report) {
    let o = &config.outbox;
    report.range(
        "outbox.retention_days",
        u64::from(o.retention_days),
        1,
        3650,
    );
    report.range("outbox.poll_interval_ms", o.poll_interval_ms, 50, 60_000);
}

fn downloads(config: &AppConfig, report: &mut Report) {
    let d = &config.downloads;
    report.range(
        "downloads.flush_interval_secs",
        d.flush_interval_secs,
        1,
        3600,
    );
    // A monthly view needs a full month of daily counters plus a few days of slack.
    report.range(
        "downloads.retention_days",
        u64::from(d.retention_days),
        35,
        3650,
    );
}

fn egress(config: &AppConfig, report: &mut Report) {
    let e = &config.egress;
    http_policy(config, "egress.registries", &e.registries, report);
    http_policy(config, "egress.webhooks", &e.webhooks, report);
    http_policy(config, "egress.scanners", &e.scanners, report);
    http_policy(config, "egress.probe", &e.probe, report);
    smtp(&e.smtp, report);
    for host in &e.proxy.allowed_hosts {
        if !is_plain_host(host) {
            report.err(
                "egress.proxy.allowed_hosts",
                "entries must be host names or IP addresses without scheme, port or wildcard",
            );
        }
    }
}

fn http_policy(config: &AppConfig, base: &str, policy: &EgressPolicyConfig, report: &mut Report) {
    let key = |name: &str| format!("{base}.{name}");
    let schemes = key("allowed_schemes");
    for scheme in &policy.allowed_schemes {
        if !matches!(scheme.as_str(), "http" | "https") {
            report.err(&schemes, "only `http` and `https` are supported");
        }
    }
    let https = policy.allowed_schemes.iter().any(|s| s == "https");
    let http = policy.allowed_schemes.iter().any(|s| s == "http");
    if !https {
        report.err(&schemes, "must include `https`");
    }
    if http && policy.allow_insecure_http_hosts.is_empty() {
        report.err(
            &schemes,
            format!("`http` needs the hosts it applies to in {base}.allow_insecure_http_hosts"),
        );
    }
    if !http && !policy.allow_insecure_http_hosts.is_empty() {
        report.err(
            &key("allow_insecure_http_hosts"),
            format!("has entries but `http` is not in {base}.allowed_schemes"),
        );
    }
    if http && config.is_production() {
        report.warn(&schemes, "plain http targets are allowed in production");
    }
    host_entries(
        &key("allow_insecure_http_hosts"),
        &policy.allow_insecure_http_hosts,
        false,
        report,
    );
    host_entries(&key("allowed_hosts"), &policy.allowed_hosts, true, report);
    ports(&key("allowed_ports"), &policy.allowed_ports, report);
    cidrs(
        &key("allow_private_cidrs"),
        &policy.allow_private_cidrs,
        report,
    );
}

fn smtp(policy: &EgressSmtpConfig, report: &mut Report) {
    host_entries(
        "egress.smtp.allowed_hosts",
        &policy.allowed_hosts,
        false,
        report,
    );
    ports("egress.smtp.allowed_ports", &policy.allowed_ports, report);
    cidrs(
        "egress.smtp.allow_private_cidrs",
        &policy.allow_private_cidrs,
        report,
    );
}

fn host_entries(key: &str, hosts: &[String], wildcard: bool, report: &mut Report) {
    for host in hosts {
        let name = if wildcard {
            host.strip_prefix("*.").unwrap_or(host)
        } else {
            host.as_str()
        };
        if !is_plain_host(name) {
            report.err(
                key,
                if wildcard {
                    "entries must be host names, or `*.example.com` patterns"
                } else {
                    "entries must be host names or IP addresses without scheme, port or wildcard"
                },
            );
        }
    }
}

fn ports(key: &str, ports: &[u16], report: &mut Report) {
    if ports.is_empty() {
        report.err(key, "must list at least one port");
    }
    if ports.contains(&0) {
        report.err(key, "port 0 is not a destination");
    }
}

/// The same rule as `upstream.allow_private_cidrs`: the allowlist exists for internal
/// targets and can never legitimize loopback or the instance metadata address.
fn cidrs(key: &str, nets: &[IpNet], report: &mut Report) {
    for net in nets {
        private_cidr(key, net, report);
    }
}
