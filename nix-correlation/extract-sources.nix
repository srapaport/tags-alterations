# extract-sources.nix

let
  pkgs = import <nixpkgs> {
    config = {
      allowUnfree         = true;
      allowBroken         = true;
      allowInsecure       = true;
      allowUnsupportedSystem = true;
    };
  };

  lib = pkgs.lib;

  # ------------------------------------------------------------------ #
  # Helpers
  # ------------------------------------------------------------------ #

  # Safely attempt to read a string attribute, return null if absent/unevalable
  tryStr = attrset: key:
    let v = builtins.tryEval (attrset.${key} or null);
    in if v.success then v.value else null;

  # Detect whether a rev string looks like a commit hash or a tag name
  revType = rev:
    if rev == null then "none"
    # Full SHA-1 (git)
    else if builtins.match "[0-9a-fA-F]{40}"    rev != null then "commit-sha1"
    # Abbreviated SHA (≥7 hex chars — less reliable but common)
    else if builtins.match "[0-9a-fA-F]{7,39}"  rev != null then "commit-sha1-abbrev"
    # SHA-256 (git's new object format / some fetchers)
    else if builtins.match "[0-9a-fA-F]{64}"    rev != null then "commit-sha256"
    # Subversion revision numbers
    else if builtins.match "[0-9]+"             rev != null then "svn-revnum"
    else "tag";  # <── the interesting case for supply-chain research

  # Common output-hash field (fixed-output derivations store it here)
  getHash = src:
    src.outputHash  or
    src.sha256      or
    src.sha512      or
    src.md5         or
    src.hash        or
    null;

  # Build the canonical origin URL that matches your DB's origin_url column
  # so the Python join is trivial
  buildOriginUrl = type: attrs:
    let
      scheme = "https";
    in
    if      type == "fetchFromGitHub"     then "${scheme}://github.com/${attrs.owner}/${attrs.repo}"
    else if type == "fetchFromGitLab"     then "${scheme}://${attrs.domain}/${attrs.owner}/${attrs.repo}"
    else if type == "fetchFromGitea"      then "${scheme}://${attrs.domain}/${attrs.owner}/${attrs.repo}"
    else if type == "fetchFromGogs"       then "${scheme}://${attrs.domain}/${attrs.owner}/${attrs.repo}"
    else if type == "fetchFromBitbucket"  then "${scheme}://bitbucket.org/${attrs.owner}/${attrs.repo}"
    else if type == "fetchFromSourcehut"  then "${scheme}://git.sr.ht/~${attrs.owner}/${attrs.repo}"
    else if type == "fetchgit"            then attrs.url or null
    else if type == "fetchgitLocal"       then attrs.url or null
    else if type == "fetchsvn"            then attrs.url or null
    else if type == "fetchhg"             then attrs.url or null
    else if type == "fetchurl"            then attrs.url or null
    else if type == "fetchzip"            then attrs.url or null
    else if type == "fetchTarball"        then attrs.url or null
    else null;

  # ------------------------------------------------------------------ #
  # Per-fetcher extractors
  # Each returns a normalised attrset so downstream code is uniform
  # ------------------------------------------------------------------ #

  extractors = {

    # ---- GitHub -------------------------------------------------------
    fetchFromGitHub = src: rec {
      type       = "fetchFromGitHub";
      domain     = "github.com";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then "refs/tags/${rev}" else null;
      hash       = getHash src;
      # fetchFromGitHub can use submodules
      fetchSubmodules = src.fetchSubmodules or false;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = "${origin_url}/archive/${rev}.tar.gz";
    };

    # ---- GitLab (self-hosted instances included via `domain`) ---------
    fetchFromGitLab = src: rec {
      type       = "fetchFromGitLab";
      domain     = src.domain or "gitlab.com";
      # GitLab supports nested groups: owner may contain slashes
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then "refs/tags/${rev}" else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = "${origin_url}/-/archive/${rev}/${repo}-${rev}.tar.gz";
    };

    # ---- Gitea (self-hosted, e.g. codeberg.org runs Forgejo/Gitea) ----
    fetchFromGitea = src: rec {
      type       = "fetchFromGitea";
      domain     = src.domain or "gitea.com";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then "refs/tags/${rev}" else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = "https://${domain}/${owner}/${repo}/archive/${rev}.tar.gz";
    };

    # ---- Forgejo (Codeberg & others — same API shape as Gitea) --------
    # NixOS/nixpkgs added fetchFromForgejo around 2024
    fetchFromForgejo = src: rec {
      type       = "fetchFromForgejo";
      domain     = src.domain or "codeberg.org";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then "refs/tags/${rev}" else null;
      hash       = getHash src;
      origin_url = buildOriginUrl "fetchFromGitea" { inherit domain owner repo; };
      fetch_url  = "https://${domain}/${owner}/${repo}/archive/${rev}.tar.gz";
    };

    # ---- Gogs ----------------------------------------------------------
    fetchFromGogs = src: rec {
      type       = "fetchFromGogs";
      domain     = src.domain or "gogs.io";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then "refs/tags/${rev}" else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = "https://${domain}/${owner}/${repo}/archive/${rev}.tar.gz";
    };

    # ---- Bitbucket (git AND hg repos) ----------------------------------
    fetchFromBitbucket = src: rec {
      type       = "fetchFromBitbucket";
      domain     = "bitbucket.org";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then "refs/tags/${rev}" else null;
      hash       = getHash src;
      # Bitbucket still hosts some Mercurial repos
      vcs        = src.vcs or "git";
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  =
        if vcs == "hg"
        then "https://bitbucket.org/${owner}/${repo}/get/${rev}.tar.gz"
        else "https://bitbucket.org/${owner}/${repo}/get/${rev}.tar.gz";
    };

    # ---- SourceHut (git.sr.ht) -----------------------------------------
    fetchFromSourcehut = src: rec {
      type       = "fetchFromSourcehut";
      domain     = src.domain or "git.sr.ht";
      owner      = tryStr src "owner";   # without the leading ~
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then "refs/tags/${rev}" else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = "https://${domain}/~${owner}/${repo}/archive/${rev}.tar.gz";
    };

    # ---- Generic fetchgit (bare git clone) -----------------------------
    fetchgit = src: rec {
      type       = "fetchgit";
      url        = tryStr src "url";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then "refs/tags/${rev}" else null;
      hash       = getHash src;
      fetchSubmodules = src.fetchSubmodules or false;
      origin_url = url;
      fetch_url  = url;
    };

    # ---- fetchgitLocal (local path, rarely interesting for supply chain)
    fetchgitLocal = src: rec {
      type       = "fetchgitLocal";
      url        = tryStr src "url";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then "refs/tags/${rev}" else null;
      hash       = getHash src;
      origin_url = url;
      fetch_url  = url;
    };

    # ---- SVN -----------------------------------------------------------
    fetchsvn = src: rec {
      type       = "fetchsvn";
      url        = tryStr src "url";
      # SVN uses integer revision numbers, not hashes/tags
      rev        = tryStr src "rev";
      rev_type   = revType rev;    # will be "svn-revnum" for numeric revs
      tag_name   = null;           # SVN has no git-style tags
      hash       = getHash src;
      origin_url = url;
      fetch_url  = url;
    };

    # ---- Mercurial -----------------------------------------------------
    fetchhg = src: rec {
      type       = "fetchhg";
      url        = tryStr src "url";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" then rev else null;  # hg uses plain tag names
      hash       = getHash src;
      origin_url = url;
      fetch_url  = url;
    };

    # ---- fetchurl (plain HTTP/FTP download) ----------------------------
    fetchurl = src: rec {
      type       = "fetchurl";
      url        = src.url or (builtins.head (src.urls or [""]));
      urls       = src.urls or (if src ? url then [ src.url ] else []);
      rev        = null;
      rev_type   = "none";
      tag_name   = null;
      hash       = getHash src;
      origin_url = url;
      fetch_url  = url;
    };

    # ---- fetchzip (tarball, same attrs as fetchurl mostly) -------------
    fetchzip = src: rec {
      type       = "fetchzip";
      url        = tryStr src "url";
      rev        = null;
      rev_type   = "none";
      tag_name   = null;
      hash       = getHash src;
      origin_url = url;
      fetch_url  = url;
    };

    # ---- fetchTarball (builtins) ---------------------------------------
    fetchTarball = src: rec {
      type       = "fetchTarball";
      url        = tryStr src "url";
      rev        = null;
      rev_type   = "none";
      tag_name   = null;
      hash       = getHash src;
      origin_url = url;
      fetch_url  = url;
    };

  };

  # ------------------------------------------------------------------ #
  # Detection logic
  # Order matters: most specific checks first
  # ------------------------------------------------------------------ #

  # Does a string attr contain a substring?
  hasIn = src: key: sub:
    let v = src.${key} or null;
    in v != null && lib.hasInfix sub v;

  classifySrc = src:
    # Guard: src must be an attrset (some packages set src to a path/string)
    if !builtins.isAttrs src then { type = "non-attrset"; }

    # ── forge fetchers (owner + repo + rev is the shared signature) ──
    else if src ? owner && src ? repo && src ? rev then
      let
        domain = src.domain or "";
        url    = src.url    or "";
      in
           if lib.hasInfix "github.com"    url
           || (domain == "" && !(lib.hasInfix "." domain))
                                                   then extractors.fetchFromGitHub    src
      else if lib.hasInfix "gitlab"        url
           || lib.hasInfix "gitlab"        domain  then extractors.fetchFromGitLab    src
      else if lib.hasInfix "bitbucket.org" url
           || lib.hasInfix "bitbucket.org" domain  then extractors.fetchFromBitbucket src
      else if lib.hasInfix "sr.ht"         url
           || lib.hasInfix "sr.ht"         domain  then extractors.fetchFromSourcehut src
      else if lib.hasInfix "codeberg.org"  url
           || lib.hasInfix "codeberg.org"  domain  then extractors.fetchFromForgejo   src
      else if lib.hasInfix "forgejo"       url
           || lib.hasInfix "forgejo"       domain  then extractors.fetchFromForgejo   src
      else if lib.hasInfix "gogs"          url
           || lib.hasInfix "gogs"          domain  then extractors.fetchFromGogs       src
      # Gitea is the fallback for owner+repo+domain packages
      else if src ? domain                         then extractors.fetchFromGitea     src
      # No domain and no recognisable URL: assume GitHub (most common in nixpkgs)
      else                                              extractors.fetchFromGitHub    src

    # ── vcs fetchers identified by other means ──
    else if src ? url && src ? rev && src ? svnRoot  then extractors.fetchsvn        src
    else if src ? url && src ? rev && src ? hgHash   then extractors.fetchhg         src
    else if src ? url && src ? rev                   then extractors.fetchgit        src

    # ── archive fetchers ──
    else if src ? urls                               then extractors.fetchurl        src
    else if src ? url then
      let url = src.url;
      in   if lib.hasSuffix ".zip"     url
           || lib.hasSuffix ".tar.gz"  url
           || lib.hasSuffix ".tar.bz2" url
           || lib.hasSuffix ".tar.xz"  url
           || lib.hasSuffix ".tgz"     url         then extractors.fetchzip         src
      else                                              extractors.fetchurl         src

    else { type = "unknown"; };

  # ------------------------------------------------------------------ #
  # Safe per-package extraction
  # ------------------------------------------------------------------ #

  safeExtract = attrPath: pkg:
    let
      isCandidateResult = builtins.tryEval (lib.isDerivation pkg && pkg ? src);
      isCandidate = isCandidateResult.success && isCandidateResult.value;
    in
    if !isCandidate then null
    else
      let
        srcResult = builtins.tryEval pkg.src;
      in
      if !srcResult.success || srcResult.value == null then null
      else
        let
          src             = srcResult.value;
          classifyResult  = builtins.tryEval (classifySrc src);
          srcInfo         = if classifyResult.success
                            then classifyResult.value
                            else { type = "eval-error"; };
          versionResult   = builtins.tryEval (pkg.version or null);
          pnameResult     = builtins.tryEval (pkg.pname   or null);
        in
        if srcInfo.type == "unknown" || srcInfo.type == "non-attrset"
           || srcInfo.type == "eval-error"
        then null
        else {
          attr_path = attrPath;
          pname     = if pnameResult.success   then pnameResult.value   else null;
          version   = if versionResult.success then versionResult.value else null;
          src       = srcInfo;
        };

  # ------------------------------------------------------------------ #
  # Scopes to crawl
  # ------------------------------------------------------------------ #

  scopes = [
    # language ecosystems
    { name = "python3Packages";    set = pkgs.python3Packages;    }
    { name = "python311Packages";  set = pkgs.python311Packages;  }
    { name = "python312Packages";  set = pkgs.python312Packages;  }
    { name = "python313Packages";  set = pkgs.python313Packages;  }
    { name = "nodePackages";       set = pkgs.nodePackages;       }
    { name = "perlPackages";       set = pkgs.perlPackages;       }
    { name = "rubyPackages";       set = pkgs.rubyPackages;       }
    { name = "haskellPackages";    set = pkgs.haskellPackages;    }
    { name = "ocamlPackages";      set = pkgs.ocamlPackages;      }
    { name = "rPackages";          set = pkgs.rPackages;          }
    { name = "luaPackages";        set = pkgs.luaPackages;        }
    { name = "phpPackages";        set = pkgs.phpPackages;        }
    { name = "emacsPackages";      set = pkgs.emacsPackages;      }
    { name = "vimPlugins";         set = pkgs.vimPlugins;         }
    { name = "linuxPackages";      set = pkgs.linuxPackages;      }
    # forge-specific aggregates
    { name = "gnome";              set = pkgs.gnome;              }
    { name = "xfce";               set = pkgs.xfce;               }
    { name = "kde";                set = pkgs.kdePackages;        }
    { name = "plasma5Packages";    set = pkgs.plasma5Packages;    }
  ];

  # Crawl one attribute set, prefix every key with `prefix.`
  crawlScope = prefix: attrset:
    let
      namesResult = builtins.tryEval (builtins.attrNames attrset);
      names       = if namesResult.success then namesResult.value else [];
    in
    lib.foldl'
      (acc: name:
        let
          attrPath  = "${prefix}.${name}";
          pkgResult = builtins.tryEval attrset.${name};
          pkg       = if pkgResult.success then pkgResult.value else null;
          extracted = if pkg == null then null
                      else
                        let r = builtins.tryEval (safeExtract attrPath pkg);
                        in if r.success then r.value else null;
        in
        if extracted != null
        then acc // { ${attrPath} = extracted; }
        else acc
      )
      {}
      names;

  # Top-level crawl
  topLevel =
    let names = builtins.attrNames pkgs;
    in lib.foldl'
      (acc: name:
        let
          pkgResult = builtins.tryEval pkgs.${name};
          pkg       = if pkgResult.success then pkgResult.value else null;
          extracted = if pkg == null then null
                      else
                        let r = builtins.tryEval (safeExtract name pkg);
                        in if r.success then r.value else null;
        in
        if extracted != null
        then acc // { ${name} = extracted; }
        else acc
      )
      {}
      names;

  # Scoped crawl (with tryEval guard around the set itself)
  scopedAll =
    lib.foldl'
      (acc: entry:
        let
          setResult = builtins.tryEval entry.set;
        in
        if !setResult.success || !builtins.isAttrs setResult.value
        then acc
        else acc // (crawlScope entry.name setResult.value)
      )
      {}
      scopes;

in
topLevel // scopedAll
