//! Finding the folders a server actually serves.
//!
//! "Open this site's files" is the thing a person wants from a file browser attached to a
//! monitoring tool, and answering it by guessing `/var/www/<domain>` is wrong often
//! enough to be useless: real deployments live under `/home`, `/srv`, `/opt`, or a
//! release directory behind a symlink.
//!
//! So the web server's own configuration is read instead. Whatever nginx or Apache says
//! it is serving is what gets listed — including the virtual host nobody remembers
//! setting up, which is exactly the one worth seeing.
//!
//! These are parsers, not validators. A configuration file that this cannot read yields
//! fewer roots, never a wrong one: the fallback is the default start path, and a person
//! can always navigate.

/// Where nginx keeps its enabled virtual hosts, in the order worth trying.
///
/// Debian and Ubuntu use `sites-enabled`; RHEL, Alpine and most container images use
/// `conf.d`. Both are read, because a machine can have both.
pub const NGINX_CONFIG_DIRS: &[&str] = &["/etc/nginx/sites-enabled", "/etc/nginx/conf.d"];

/// Where Apache keeps its enabled virtual hosts.
pub const APACHE_CONFIG_DIRS: &[&str] = &["/etc/apache2/sites-enabled", "/etc/httpd/conf.d"];

/// A folder that is being served, and the names it is served under.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SiteRoot {
    /// The document root, exactly as the configuration gives it.
    pub path: String,
    /// The host names served from it, in the order the configuration lists them.
    pub names: Vec<String>,
    /// The file this was found in, so a puzzling entry can be traced back.
    pub source: String,
}

impl SiteRoot {
    /// A label for the interface: the first name, or the path when there is none.
    pub fn label(&self) -> &str {
        self.names
            .first()
            .map(String::as_str)
            .unwrap_or(self.path.as_str())
    }
}

/// Extracts document roots from one nginx configuration file.
pub fn parse_nginx_roots(config: &str, source: &str) -> Vec<SiteRoot> {
    let mut found: Vec<SiteRoot> = Vec::new();
    let mut depth = 0usize;
    let mut server: Option<(usize, PartialSite)> = None;
    let mut words: Vec<String> = Vec::new();

    for token in tokenise(config) {
        match token {
            Token::Word(word) => words.push(word),
            Token::Open => {
                depth += 1;
                // A `server { … }` inside another one is not a thing nginx has, so the
                // outermost wins and nested blocks (`location`, `if`) are just passed
                // through — their directives still belong to the enclosing server.
                if server.is_none() && words.first().map(String::as_str) == Some("server") {
                    server = Some((depth, PartialSite::default()));
                }
                words.clear();
            }
            Token::Close => {
                if let Some((started_at, site)) = server.take() {
                    if started_at == depth {
                        if let Some(root) = site.into_site(source) {
                            found.push(root);
                        }
                    } else {
                        server = Some((started_at, site));
                    }
                }
                depth = depth.saturating_sub(1);
                words.clear();
            }
            Token::Semi => {
                if let Some((_, site)) = server.as_mut() {
                    site.directive(&words);
                }
                words.clear();
            }
        }
    }

    dedupe(found)
}

/// Extracts document roots from one Apache configuration file.
///
/// Apache's syntax is line-oriented and needs none of nginx's brace tracking: a
/// `DocumentRoot` outside a virtual host is still a document root.
pub fn parse_apache_roots(config: &str, source: &str) -> Vec<SiteRoot> {
    let mut found = Vec::new();
    let mut site = PartialSite::default();

    for raw in config.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        if line.eq_ignore_ascii_case("</VirtualHost>") {
            if let Some(root) = std::mem::take(&mut site).into_site(source) {
                found.push(root);
            }
            continue;
        }
        if line.to_ascii_lowercase().starts_with("<virtualhost") {
            site = PartialSite::default();
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(directive) = parts.next() else {
            continue;
        };
        let values: Vec<String> = parts.map(|v| v.trim_matches('"').to_owned()).collect();

        if directive.eq_ignore_ascii_case("DocumentRoot") {
            site.root = values.first().cloned();
        } else if directive.eq_ignore_ascii_case("ServerName")
            || directive.eq_ignore_ascii_case("ServerAlias")
        {
            site.names.extend(values);
        }
    }

    // A file that never closed a virtual host still had directives worth keeping.
    if let Some(root) = site.into_site(source) {
        found.push(root);
    }
    dedupe(found)
}

/// A site under construction, before it is known whether it has a root at all.
#[derive(Debug, Default)]
struct PartialSite {
    root: Option<String>,
    names: Vec<String>,
}

impl PartialSite {
    fn directive(&mut self, words: &[String]) {
        let Some((name, values)) = words.split_first() else {
            return;
        };
        match name.as_str() {
            // The first `root` wins: nginx's server-level directive is written before the
            // `location` blocks that may override it for one path, and the server-level
            // one is what "the site's folder" means.
            "root" if self.root.is_none() => self.root = values.first().cloned(),
            "server_name" => self.names.extend(
                values
                    .iter()
                    // `_` is nginx's catch-all placeholder, not a host name.
                    .filter(|value| value.as_str() != "_")
                    .cloned(),
            ),
            _ => {}
        }
    }

    fn into_site(self, source: &str) -> Option<SiteRoot> {
        // A server block with no root serves nothing this browser can open — a redirect,
        // or a proxy. Listing it would offer a folder that does not exist.
        let path = self.root?;
        if !path.starts_with('/') {
            // A relative root is resolved against nginx's prefix, which is not knowable
            // from the file. Better to omit it than to invent a location.
            return None;
        }
        Some(SiteRoot {
            path: path.trim_end_matches('/').to_owned(),
            names: self.names,
            source: source.to_owned(),
        })
    }
}

/// Merges sites sharing a root, which is the normal shape of an HTTP/HTTPS pair.
fn dedupe(found: Vec<SiteRoot>) -> Vec<SiteRoot> {
    let mut merged: Vec<SiteRoot> = Vec::new();
    for site in found {
        match merged
            .iter_mut()
            .find(|existing| existing.path == site.path)
        {
            Some(existing) => {
                // Order is preserved rather than sorted: the first name is the one the
                // interface shows, and the configuration's own order is the useful one.
                for name in site.names {
                    if !existing.names.contains(&name) {
                        existing.names.push(name);
                    }
                }
            }
            None => merged.push(site),
        }
    }
    merged
}

/// The pieces of nginx's syntax that matter here.
enum Token {
    Word(String),
    Open,
    Close,
    Semi,
}

/// Splits a configuration into words and block punctuation.
///
/// Character-wise rather than line-wise because nginx does not care about newlines:
/// `server { root /var/www; }` on one line is as valid as the same across four.
fn tokenise(config: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = config.chars().peekable();

    let flush = |word: &mut String, tokens: &mut Vec<Token>| {
        if !word.is_empty() {
            tokens.push(Token::Word(std::mem::take(word)));
        }
    };

    while let Some(c) = chars.next() {
        match c {
            '#' => {
                flush(&mut word, &mut tokens);
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' | '\'' => {
                // A quoted value is one word even with spaces in it.
                let quote = c;
                for c in chars.by_ref() {
                    if c == quote {
                        break;
                    }
                    word.push(c);
                }
            }
            '{' => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Open);
            }
            '}' => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Close);
            }
            ';' => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Semi);
            }
            c if c.is_whitespace() => flush(&mut word, &mut tokens),
            c => word.push(c),
        }
    }
    flush(&mut word, &mut tokens);
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBIAN_DEFAULT: &str = r#"
server {
    listen 80 default_server;
    root /var/www/html;
    index index.html index.htm;
    server_name _;
    location / {
        try_files $uri $uri/ =404;
    }
}
"#;

    const REAL_SITE: &str = r#"
# managed by Certbot
server {
    server_name example.ru www.example.ru;
    root /home/deploy/example/public;

    location ~ \.php$ {
        root /should/not/win;
        fastcgi_pass unix:/run/php/php8.2-fpm.sock;
    }

    listen 443 ssl;
    ssl_certificate /etc/letsencrypt/live/example.ru/fullchain.pem;
}

server {
    listen 80;
    server_name example.ru www.example.ru;
    return 301 https://$host$request_uri;
}
"#;

    #[test]
    fn a_document_root_is_found_with_the_names_it_serves() {
        let sites = parse_nginx_roots(REAL_SITE, "/etc/nginx/sites-enabled/example");
        assert_eq!(sites.len(), 1, "{sites:#?}");
        assert_eq!(sites[0].path, "/home/deploy/example/public");
        assert_eq!(sites[0].names, ["example.ru", "www.example.ru"]);
        assert_eq!(sites[0].label(), "example.ru");
    }

    #[test]
    fn a_root_inside_a_location_does_not_replace_the_sites_own() {
        // A `location ~ \.php$ { root … }` is a PHP handler, not the site's folder.
        let sites = parse_nginx_roots(REAL_SITE, "x");
        assert!(
            sites.iter().all(|s| s.path != "/should/not/win"),
            "{sites:#?}"
        );
    }

    #[test]
    fn a_redirect_only_server_offers_no_folder() {
        // The second block in REAL_SITE has no root; listing it would offer a directory
        // that does not exist.
        let sites = parse_nginx_roots(REAL_SITE, "x");
        assert_eq!(sites.len(), 1);
    }

    #[test]
    fn the_catch_all_placeholder_is_not_treated_as_a_host_name() {
        let sites = parse_nginx_roots(DEBIAN_DEFAULT, "/etc/nginx/sites-enabled/default");
        assert_eq!(sites[0].path, "/var/www/html");
        assert!(sites[0].names.is_empty(), "{:?}", sites[0].names);
        // With no name to show, the folder itself is the label.
        assert_eq!(sites[0].label(), "/var/www/html");
    }

    #[test]
    fn an_http_and_https_pair_sharing_a_folder_is_listed_once() {
        let config = r#"
server { listen 80; server_name a.ru; root /srv/a; }
server { listen 443 ssl; server_name a.ru www.a.ru; root /srv/a; }
"#;
        let sites = parse_nginx_roots(config, "x");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].names, ["a.ru", "www.a.ru"]);
    }

    #[test]
    fn newlines_are_not_part_of_the_syntax() {
        // nginx does not care where the line breaks are, so neither may this.
        let one_line = "server { root /srv/x; server_name x.ru; }";
        let sites = parse_nginx_roots(one_line, "x");
        assert_eq!(sites[0].path, "/srv/x");
        assert_eq!(sites[0].names, ["x.ru"]);
    }

    #[test]
    fn a_commented_out_site_is_not_listed() {
        let config = "server { root /srv/live; }\n# server { root /srv/dead; }\n";
        let sites = parse_nginx_roots(config, "x");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].path, "/srv/live");
    }

    #[test]
    fn a_trailing_slash_is_dropped_so_paths_compare_equal() {
        let sites = parse_nginx_roots("server { root /srv/x/; server_name a; }", "x");
        assert_eq!(sites[0].path, "/srv/x");
    }

    #[test]
    fn a_relative_root_is_omitted_rather_than_invented() {
        // It resolves against nginx's compiled-in prefix, which this cannot know.
        let sites = parse_nginx_roots("server { root html; server_name a; }", "x");
        assert!(sites.is_empty(), "{sites:#?}");
    }

    #[test]
    fn nonsense_yields_nothing_rather_than_a_wrong_answer() {
        for input in ["", "{{{{", "}}}}", "server {", "root /srv/x;"] {
            let sites = parse_nginx_roots(input, "x");
            assert!(sites.is_empty(), "{input:?} produced {sites:#?}");
        }
    }

    #[test]
    fn an_apache_virtual_host_is_read_too() {
        let config = r#"
<VirtualHost *:80>
    ServerName  example.ru
    ServerAlias www.example.ru
    DocumentRoot /var/www/example
    # DocumentRoot /var/www/old
</VirtualHost>
"#;
        let sites = parse_apache_roots(config, "/etc/apache2/sites-enabled/example.conf");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].path, "/var/www/example");
        assert_eq!(sites[0].names, ["example.ru", "www.example.ru"]);
    }
}
