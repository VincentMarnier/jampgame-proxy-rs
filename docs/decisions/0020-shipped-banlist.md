# D-020 — Shipped cloud/hosting/VPN ban list

Date: 2026-11-03. Status: accepted. Feature: `rust/proxy_bans.txt`,
`tools/update_banlist.py`.

## Context

D-017 made `proxy_sv_banFile` the source of truth for IP bans and made the
format a plain list of IPv4 addresses and CIDR ranges, explicitly sized for
blocklists ("A VPN blocklist can be dropped straight into this file"). That
left two practical gaps for an operator:

- there is no obvious place to get a list that covers cloud, hosting and VPN
  space without assembling it by hand;
- a hand-assembled list has no provenance, no way to reproduce it, and rots
  silently as providers add ranges.

The proxy itself must not detect VPNs (D-017: detection is out of scope), so
the list is data, not code. Data that ships needs a generator and a refresh
path or it becomes stale folklore.

## Decision

Ship a generated snapshot at `rust/proxy_bans.txt`, next to the module it
belongs to, plus the generator that produced it.

### Sources

Only sources that publish machine-readable ranges are used, so the output can
be rebuilt from the URLs in its own header:

| Source | Feed |
| --- | --- |
| AWS | `ip-ranges.json` (`ip_prefix`) |
| Google Cloud | `cloud.json` (`ipv4Prefix`) |
| Azure | "Azure IP Ranges and Service Tags – Public Cloud" JSON (link scraped from the Microsoft download page, since its filename is dated) |
| Oracle Cloud | `public_ip_ranges.json` |
| DigitalOcean | self-published geofeed |
| Linode/Akamai | self-published geofeed |
| Vultr | Constant.com geofeed (AS20473) |
| X4BNet | `lists_vpn` `output/datacenter/ipv4.txt` |

The providers' own feeds are authoritative for their space. The long tail
(OVH, Hetzner, Contabo, Scaleway, the smaller VPN brands) does not publish
feeds, so it comes from X4BNet's datacenter aggregate, which is rebuilt from
current ASN ownership and includes VPN provider ASNs. X4BNet's `ipv4.txt`
legacy path is the same data but is scheduled for removal in 2027; the
generator uses the explicit `output/datacenter/ipv4.txt` path.

### Generation rules

`tools/update_banlist.py` (Python 3 standard library only):

1. fetches every source; **fails** rather than writing a partial list unless
   `--allow-missing` is passed;
2. keeps IPv4 only and drops `/0`;
3. subtracts RFC 6890 special-purpose space (`0/8`, `10/8`, `100.64/10`,
   `127/8`, `169.254/16`, `172.16/12`, `192.0.0/24`, `192.0.2/24`,
   `192.88.99/24`, `192.168/16`, `198.18/15`, `198.51.100/24`,
   `203.0.113/24`, `224/4`, `240/4`);
4. merges overlapping and adjacent ranges with `ipaddress.collapse_addresses`,
   so the file is the smallest equivalent CIDR set (no coverage is added by
   merging);
5. sorts the result and writes it with a provenance header (snapshot date,
   range/address counts, per-source counts and URLs);
6. **refuses to write** a list that fully contains one of a small set of
   consumer-ISP blocks (`24/8`, `70/8`–`76/8`, `78/8`–`82/8`, `84/8`, `86/8`–
   `90/8`, `92/8`, `94/8`, `95/8`, `98/8`, `99/8`, `108/8`, `174/8`); a feed
   that swallows an eyeball ASN is a broken feed, not a ban policy. Override
   with `--allow-suspect` after inspecting the source.

The file is a snapshot, not a live feed. `--date` pins the header date so a
regeneration for the same inputs is byte-identical.

### Size of the current snapshot (2026-11-03)

- 43,953 ranges, 417,087,463 addresses (≈9.7 % of IPv4), 689,496 bytes.
- By address space, the list is 37.7 % Amazon, 19.4 % Microsoft, 5.3 % Google
  Cloud, 2.9 % unrouted space inside provider aggregates, then Alibaba,
  Akamai, Tencent, Oracle, OVH, IBM SoftLayer, Hetzner, DigitalOcean, Vultr,
  Hostinger and the rest of the hosting/VPN tail (measured by joining the file
  against the iptoasn `ip2asn-v4` snapshot of the same day).
- Consumer ISP space is not covered in bulk: the largest eyeball name in that
  join is a 0.02 % share (Charter), i.e. isolated business/VPN ranges inside an
  ISP's allocation, not the ISP.

This is a deliberately aggressive policy: an ordinary player behind a
corporate VPN or a cloud egress is rejected.

## Verification

- `cargo test` (337 unit tests) includes
  `banfile::tests::the_shipped_ban_list_parses`, which parses
  `rust/proxy_bans.txt` through the real parser and asserts: no invalid
  lines, more than 10,000 entries, AWS/Azure/Google Cloud space covered, and
  no private or loopback space covered.
- Real engine (1.011 `linuxjampded`, i386 runtime image, proxy built by
  `rust/scripts/build.sh`, `proxy_sv_enableBanFile 1`, ban file mounted at the
  server's working directory):
  - startup printed `proxy: ban file proxy_bans.txt loaded (43953 entries)`
    with no invalid-line warnings;
  - a probe from `3.0.0.3` (inside the list's `3.0.0.0/8`) received
    `print\nBanned.`;
  - a probe from `172.30.0.3` (not listed) passed the gate and reached the game
    (`Failed userinfo validation: model field not found`, D-017's expected
    un-banned result).

## Consequences / limits

- Refreshing is manual: re-run `tools/update_banlist.py` and commit the diff
  (or pass it to the server). Every release attaches the file as an asset, so
  the asset is a snapshot too.
- IPv4 only, matching the engine and D-017.
- `addip`/`removeip` rewrite the file in canonical form and drop the header;
  `reloadbans` never rewrites it. A server that uses `addip` on this list will
  lose the provenance header — that is D-017's documented behaviour.
- The connect gate scans the loaded vector linearly (D-017), which at this size
  is ~44 k masked comparisons per connect — paid only on connect, not per
  frame. The list costs ~350 KB of module memory.
- The list bans space, not behaviour: a VPN provider that rotates into fresh,
  unlisted ranges is not caught until the sources pick it up, and a legitimate
  player behind a listed range cannot join.
- Special-use space is stripped, so private LAN clients (already exempt from
  the check) and provider-internal unrouted ranges never appear.
