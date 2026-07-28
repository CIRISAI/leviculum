# lblogd(1)

## NAME

lblogd -- dev blog server, on the web and on NomadNet

## SYNOPSIS

**lblogd** **--config** *file*
**lblogd** **--config** *file* **--print-hash**

## DESCRIPTION

**lblogd** serves a directory of Markdown posts on two sides at once: as a NomadNet page node over Reticulum, and as a web server on the clearnet. Posts are plain Markdown files with an optional TOML frontmatter block; adding a file and reloading the service publishes it to both sides.

The NomadNet side is a shared-instance client, so a Reticulum daemon must already be running — either **lnsd**(1) or Python's **rnsd** — under the instance name named in the configuration file. **lblogd** exits if no daemon answers, and the packaged service restarts it until one does.

The web side either obtains its own certificate from Let's Encrypt, or runs plain behind a reverse proxy that terminates TLS. Note that the canonical page URL and the Atom feed are derived from the configured `domains` list even when certificate handling is switched off, so a deployment behind a proxy still has to set that list.

## OPTIONS

**--config** *file*
:   Path to the TOML configuration file. Required.

**--print-hash**
:   Resolve the node's destination hash and the page paths it would serve, print them, and exit without starting any server. Needs no running daemon.

## FILES

*/etc/lblogd/config.toml*
:   Configuration file installed by the Debian package. Registered as a conffile, so local edits survive upgrades.

*/var/lib/lblogd/posts/*
:   Where the packaged service reads posts from: one Markdown file per post.

*/var/lib/lblogd/*
:   Node identity and, when certificate handling is enabled, the ACME certificate cache.

## SIGNALS

**SIGHUP**
:   Re-read the posts directory. The packaged service maps `systemctl reload lblogd` onto this.

## EXIT STATUS

**lblogd** exits non-zero when the configuration cannot be loaded, and when no Reticulum daemon is reachable on the configured shared instance.

## EXAMPLES

Print the mesh address the blog would announce, without starting it:

    lblogd --config /etc/lblogd/config.toml --print-hash

Publish a post to the packaged service:

    sudo cp my-post.md /var/lib/lblogd/posts/
    sudo systemctl reload lblogd

## SEE ALSO

**lnsd**(1), **lnomad**(1)
