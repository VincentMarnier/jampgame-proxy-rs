#!/usr/bin/env python3
"""Regenerate `proxy_bans.txt`: cloud, hosting and VPN IPv4 ranges.

The proxy enforces `proxy_sv_banFile` as a plain list of IPv4 addresses and
CIDR ranges (see `rust/src/banfile.rs`). This tool builds that list from
sources that publish their ranges in machine-readable form, so the file can be
reproduced instead of inherited by copy-paste:

  - AWS           ip-ranges.json (`ip_prefix` entries)
  - Google Cloud  cloud.json (`ipv4Prefix` entries)
  - Azure         "Azure IP Ranges and Service Tags - Public Cloud" JSON
  - Oracle Cloud  public_ip_ranges.json
  - DigitalOcean  self-published geofeed
  - Linode/Akamai self-published geofeed
  - Vultr         Constant.com geofeed (AS20473)
  - X4BNet        VPN + datacenter aggregate, built from ASN ownership

X4BNet's datacenter list is the broad one: it is generated from the ASNs of
VPN providers and datacenters, which is what covers OVH, Hetzner, Contabo,
Scaleway, the smaller VPN brands and the rest of the long tail that does not
publish a feed of its own.

Each run re-fetches every source, drops special-use (RFC 6890) space, merges
overlapping and adjacent ranges into the smallest equivalent CIDR set, and
writes the file sorted. The result is a snapshot: re-run it when the sources
move.

Usage:
    tools/update_banlist.py                     # write rust/proxy_bans.txt
    tools/update_banlist.py --out /tmp/bans.txt
    tools/update_banlist.py --allow-missing     # tolerate a broken source
    tools/update_banlist.py --date 2026-11-03   # pin the header date

Requires only the Python 3 standard library.
"""

from __future__ import annotations

import argparse
import csv
import datetime
import io
import ipaddress
import json
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO_ROOT / "rust" / "proxy_bans.txt"

AWS = "https://ip-ranges.amazonaws.com/ip-ranges.json"
GCP = "https://www.gstatic.com/ipranges/cloud.json"
AZURE_PAGE = "https://www.microsoft.com/en-us/download/confirmation.aspx?id=56519"
ORACLE = "https://docs.oracle.com/iaas/tools/public_ip_ranges.json"
DIGITALOCEAN = "https://www.digitalocean.com/geo/google.csv"
LINODE = "https://geoip.linode.com/"
VULTR = "https://geofeed.constant.com/"
X4BNET = "https://raw.githubusercontent.com/X4BNet/lists_vpn/main/output/datacenter/ipv4.txt"

USER_AGENT = "jampgame-proxy-rs banlist generator (https://github.com/VincentMarnier/jampgame-proxy-rs)"

# RFC 6890 special-purpose space. Never useful in a ban list: it is either
# unroutable (so the engine cannot see it), this host's own network, or
# documentation/test space.
SPECIAL = [
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.0.2.0/24",
    "192.88.99.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "198.51.100.0/24",
    "203.0.113.0/24",
    "224.0.0.0/4",
    "240.0.0.0/4",
]

# Consumer ISP space that must never be covered wholesale. A provider list that
# swallows one of these means the upstream source mixed an eyeball ASN into a
# hosting list; banning it would lock out ordinary players, so the run fails
# unless the operator overrides with --allow-suspect after checking.
EYEBALL = [
    "24.0.0.0/8",  # US cable (Charter/Comcast/...)
    "70.0.0.0/8",  # US cable
    "71.0.0.0/8",  # US telecom (Verizon/AT&T)
    "73.0.0.0/8",  # Comcast
    "75.0.0.0/8",  # US cable
    "76.0.0.0/8",  # Comcast
    "78.0.0.0/8",  # EU consumer ISPs
    "79.0.0.0/8",  # EU consumer ISPs (Telefonica, ...)
    "81.0.0.0/8",  # EU consumer ISPs
    "82.0.0.0/8",  # EU consumer ISPs
    "84.0.0.0/8",  # Deutsche Telekom and EU consumer ISPs
    "86.0.0.0/8",  # BT, Orange, ...
    "87.0.0.0/8",  # EU consumer ISPs
    "88.0.0.0/8",  # EU consumer ISPs
    "89.0.0.0/8",  # EU consumer ISPs
    "90.0.0.0/8",  # EU consumer ISPs
    "92.0.0.0/8",  # EU consumer ISPs
    "94.0.0.0/8",  # EU consumer ISPs
    "95.0.0.0/8",  # EU consumer ISPs
    "98.0.0.0/8",  # Comcast
    "99.0.0.0/8",  # AT&T
    "108.0.0.0/8",  # AT&T
    "174.0.0.0/8",  # US cable
]


def fetch(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=120) as response:
        return response.read()


def parse_prefixes(text: str | bytes) -> list[str]:
    """Extract every IPv4 prefix from a geofeed-style CSV."""
    body = text.decode("utf-8", "replace") if isinstance(text, bytes) else text
    prefixes = []
    for row in csv.reader(io.StringIO(body)):
        if not row:
            continue
        value = row[0].split("#")[0].strip()
        if value:
            prefixes.append(value)
    return prefixes


def azure_url() -> str:
    page = fetch(AZURE_PAGE).decode("utf-8", "replace")
    links = sorted(
        set(
            re.findall(
                r"https://download\.microsoft\.com/download/[^\"'<>\s]+\.json", page
            )
        )
    )
    for link in links:
        if "ServiceTags_Public" in link:
            return link
    raise RuntimeError(f"no ServiceTags JSON link on {AZURE_PAGE}")


def source_aws() -> list[str]:
    data = json.loads(fetch(AWS))
    return [entry["ip_prefix"] for entry in data["prefixes"]]


def source_gcp() -> list[str]:
    data = json.loads(fetch(GCP))
    return [
        entry["ipv4Prefix"] for entry in data["prefixes"] if "ipv4Prefix" in entry
    ]


def source_azure() -> list[str]:
    data = json.loads(fetch(azure_url()))
    prefixes = []
    for value in data["values"]:
        prefixes.extend(value["properties"].get("addressPrefixes", []))
    return prefixes


def source_oracle() -> list[str]:
    data = json.loads(fetch(ORACLE))
    return [cidr["cidr"] for region in data["regions"] for cidr in region["cidrs"]]


def source_digitalocean() -> list[str]:
    return parse_prefixes(fetch(DIGITALOCEAN))


def source_linode() -> list[str]:
    return parse_prefixes(fetch(LINODE))


def source_vultr() -> list[str]:
    return parse_prefixes(fetch(VULTR))


def source_x4bnet() -> list[str]:
    return parse_prefixes(fetch(X4BNET))


SOURCES = [
    ("AWS", AWS, source_aws),
    ("Google Cloud", GCP, source_gcp),
    ("Azure", AZURE_PAGE, source_azure),
    ("Oracle Cloud", ORACLE, source_oracle),
    ("DigitalOcean", DIGITALOCEAN, source_digitalocean),
    ("Linode/Akamai", LINODE, source_linode),
    ("Vultr", VULTR, source_vultr),
    ("X4BNet VPN+datacenter", X4BNET, source_x4bnet),
]


def to_ipv4(value: str) -> ipaddress.IPv4Network | None:
    try:
        net = ipaddress.ip_network(value.strip(), strict=False)
    except ValueError:
        return None
    if net.version != 4 or net.prefixlen == 0:
        return None
    return net


def subtract(net: ipaddress.IPv4Network, exclusions: list[ipaddress.IPv4Network]):
    """`net` minus every exclusion range, as a list of networks."""
    parts = [net]
    for exclusion in exclusions:
        if not exclusion.overlaps(net):
            continue
        remaining = []
        for part in parts:
            if part.subnet_of(exclusion):
                continue
            if exclusion.subnet_of(part):
                remaining.extend(part.address_exclude(exclusion))
            else:
                remaining.append(part)
        parts = remaining
    return parts


def build(
    *, allow_missing: bool, allow_suspect: bool
) -> tuple[list[ipaddress.IPv4Network], list[tuple[str, str, int]]]:
    special = [ipaddress.ip_network(value) for value in SPECIAL]
    eyeball = [] if allow_suspect else [ipaddress.ip_network(value) for value in EYEBALL]
    collected: list[ipaddress.IPv4Network] = []
    used: list[tuple[str, str, int]] = []
    failures: list[str] = []

    for name, url, source in SOURCES:
        try:
            raw = source()
        except (urllib.error.URLError, OSError, ValueError, RuntimeError) as error:
            failures.append(f"{name}: {error}")
            print(f"  ! {name}: {error}", file=sys.stderr)
            continue
        networks = [net for net in map(to_ipv4, raw) if net is not None]
        if not networks:
            failures.append(f"{name}: source returned no IPv4 prefixes")
            print(f"  ! {name}: no IPv4 prefixes", file=sys.stderr)
            continue
        collected.extend(networks)
        used.append((name, url, len(networks)))
        print(f"  {name}: {len(networks)} prefixes")

    if failures and not allow_missing:
        raise SystemExit(
            "refusing to write a partial list (re-run with --allow-missing to override):\n  "
            + "\n  ".join(failures)
        )

    cleaned: list[ipaddress.IPv4Network] = []
    for net in collected:
        cleaned.extend(subtract(net, special))
    collapsed = list(ipaddress.collapse_addresses(cleaned))
    collapsed.sort(key=lambda net: (int(net.network_address), net.prefixlen))

    suspects = [str(net) for net in eyeball if any(net.subnet_of(ban) for ban in collapsed)]
    if suspects:
        raise SystemExit(
            "sources would ban consumer ISP space outright: "
            + ", ".join(suspects)
            + " (inspect the upstream list; override with --allow-suspect only if intended)"
        )
    return collapsed, used


def render(
    networks: list[ipaddress.IPv4Network],
    used: list[tuple[str, str, int]],
    date: str,
) -> str:
    addresses = sum(net.num_addresses for net in networks)
    lines = [
        "# jampgame-proxy-rs ban list (proxy_sv_banFile)",
        "#",
        "# Cloud, hosting and VPN IPv4 ranges, generated by tools/update_banlist.py.",
        f"# Snapshot: {date} - re-run the tool to refresh it.",
        f"# {len(networks)} ranges, {addresses} addresses, {len(used)} sources.",
        "#",
        "# One IPv4 address or CIDR range per line; '#' starts a comment.",
        "# 'addip' / 'removeip' rewrite this file in canonical form and drop the",
        "# comments above; 'reloadbans' picks up hand edits without rewriting it.",
        "#",
    ]
    for name, url, count in used:
        lines.append(f"#   {count:>6}  {name}: {url}")
    lines.append("")
    for net in networks:
        lines.append(net.compressed if net.prefixlen != 32 else str(net.network_address))
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT, help=f"output path (default {DEFAULT_OUT})")
    parser.add_argument("--date", default=None, help="snapshot date in the header (default: today, UTC)")
    parser.add_argument("--allow-missing", action="store_true", help="write the list even if a source fails")
    parser.add_argument("--allow-suspect", action="store_true", help="do not fail if consumer ISP space is covered")
    arguments = parser.parse_args()

    date = arguments.date or datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")
    print(f"fetching {len(SOURCES)} sources:")
    networks, used = build(
        allow_missing=arguments.allow_missing, allow_suspect=arguments.allow_suspect
    )
    body = render(networks, used, date)
    arguments.out.write_text(body)
    print(
        f"wrote {arguments.out}: {len(networks)} ranges, "
        f"{sum(net.num_addresses for net in networks)} addresses, {len(body)} bytes"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
