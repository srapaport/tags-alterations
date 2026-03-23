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

  blacklist = [
    "anydesk"
    "buckets"
    "sundtek"
    "googleearth-pro"
    "kernel"
    "tensorrt"
    "jax-cuda12-pjrt"
    "python-modules"
    "zepp-simulator"
    "vivaldi-ffmpeg-codecs"
    "sparrow"
    "snell"
    "p3x-onenote"
    "masterpdfeditor"
    "libsciter"
    "furmark"
  ];

  isBlacklisted = name: builtins.elem name blacklist;

  # ------------------------------------------------------------------ #
  # Helpers
  # ------------------------------------------------------------------ #

  # FIX: safeBuildUrl uses a lambda to defer template evaluation
  # so null-containing strings are never interpolated
  safeBuildUrl = parts: templateFn:
    if builtins.all (p: p != null && p != "") parts
    then templateFn null
    else null;

  # Safely attempt to read a string attribute, return null if absent/unevalable
  tryStr = attrset: key:
    let v = builtins.tryEval (attrset.${key} or null);
    in if v.success then v.value else null;

  # FIX: revType with correct ordering and explicit length checks
  # to avoid misclassifying hex-looking version tags as commit hashes
  revType = rev:
    if rev == null then "none"
    else
      let
        len = builtins.stringLength rev;
      in
           if len == 40 && builtins.match "[0-9a-fA-F]{40}" rev != null then "commit-sha1"
      else if len == 64 && builtins.match "[0-9a-fA-F]{64}" rev != null then "commit-sha256"
      # Abbreviated SHA: purely hex, 7–39 chars, but exclude pure decimals
      # (those are more likely SVN revisions or numeric version tags)
      else if len >= 7 && len < 40
           && builtins.match "[0-9a-fA-F]+" rev != null
           && builtins.match "[0-9]+"       rev == null then "commit-sha1-abbrev"
      else if builtins.match "[0-9]+"       rev != null then "svn-revnum"
      else "tag";

  # Common output-hash field (fixed-output derivations store it here)
  getHash = src:
    src.outputHash or
    src.sha256     or
    src.sha512     or
    src.md5        or
    src.hash       or
    null;

  # ------------------------------------------------------------------ #
  # Canonical origin URL builder
  # ------------------------------------------------------------------ #

  buildOriginUrl = type: attrs:
    let
      owner  = attrs.owner  or null;
      repo   = attrs.repo   or null;
      domain = attrs.domain or null;
      url    = attrs.url    or null;
    in
         if type == "fetchFromGitHub"    then safeBuildUrl [owner repo]         (_: "https://github.com/${owner}/${repo}")
    else if type == "fetchFromGitLab"    then safeBuildUrl [domain owner repo]  (_: "https://${domain}/${owner}/${repo}")
    else if type == "fetchFromGitea"     then safeBuildUrl [domain owner repo]  (_: "https://${domain}/${owner}/${repo}")
    else if type == "fetchFromForgejo"   then safeBuildUrl [domain owner repo]  (_: "https://${domain}/${owner}/${repo}")
    else if type == "fetchFromGogs"      then safeBuildUrl [domain owner repo]  (_: "https://${domain}/${owner}/${repo}")
    else if type == "fetchFromBitbucket" then safeBuildUrl [owner repo]         (_: "https://bitbucket.org/${owner}/${repo}")
    else if type == "fetchFromSourcehut" then safeBuildUrl [owner repo]         (_: "https://git.sr.ht/~${owner}/${repo}")
    else if type == "fetchgit"           then url
    else if type == "fetchhg"            then url
    else if type == "fetchsvn"           then url
    else if type == "fetchurl"           then url
    else if type == "fetchzip"           then url
    else if type == "fetchTarball"       then url
    else null;

  # ------------------------------------------------------------------ #
  # Per-fetcher extractors
  # ------------------------------------------------------------------ #

  extractors = {

    # ---- GitHub -------------------------------------------------------
    fetchFromGitHub = src: rec {
      type            = "fetchFromGitHub";
      domain          = "github.com";
      owner           = tryStr src "owner";
      repo            = tryStr src "repo";
      rev             = tryStr src "rev";
      rev_type        = revType rev;
      tag_name        = if rev_type == "tag" && rev != null then rev else null;
      hash            = getHash src;
      fetchSubmodules = src.fetchSubmodules or false;
      origin_url      = buildOriginUrl type { inherit domain owner repo; };
      # FIX: no tarball URL when submodules are used (git clone path internally)
      fetch_url       =
        if fetchSubmodules then null
        else safeBuildUrl [origin_url rev] (_: "${origin_url}/archive/${rev}.tar.gz");
    };

    # ---- GitLab -------------------------------------------------------
    fetchFromGitLab = src: rec {
      type       = "fetchFromGitLab";
      domain     = src.domain or "gitlab.com";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" && rev != null then rev else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = safeBuildUrl [origin_url rev repo] (_: "${origin_url}/-/archive/${rev}/${repo}-${rev}.tar.gz");
    };

    # ---- Gitea --------------------------------------------------------
    fetchFromGitea = src: rec {
      type       = "fetchFromGitea";
      domain     = src.domain or "gitea.com";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" && rev != null then rev else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = safeBuildUrl [domain owner repo rev] (_: "https://${domain}/${owner}/${repo}/archive/${rev}.tar.gz");
    };

    # ---- Forgejo (Codeberg and others) --------------------------------
    fetchFromForgejo = src: rec {
      type       = "fetchFromForgejo";
      domain     = src.domain or "codeberg.org";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" && rev != null then rev else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = safeBuildUrl [domain owner repo rev] (_: "https://${domain}/${owner}/${repo}/archive/${rev}.tar.gz");
    };

    # ---- Gogs ---------------------------------------------------------
    fetchFromGogs = src: rec {
      type       = "fetchFromGogs";
      domain     = src.domain or "gogs.io";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" && rev != null then rev else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = safeBuildUrl [domain owner repo rev] (_: "https://${domain}/${owner}/${repo}/archive/${rev}.tar.gz");
    };

    # ---- Bitbucket ----------------------------------------------------
    fetchFromBitbucket = src: rec {
      type       = "fetchFromBitbucket";
      domain     = "bitbucket.org";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" && rev != null then rev else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      fetch_url  = safeBuildUrl [owner repo rev] (_: "https://bitbucket.org/${owner}/${repo}/get/${rev}.tar.gz");
    };

    # ---- Sourcehut ----------------------------------------------------
    fetchFromSourcehut = src: rec {
      type       = "fetchFromSourcehut";
      domain     = "git.sr.ht";
      owner      = tryStr src "owner";
      repo       = tryStr src "repo";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" && rev != null then rev else null;
      hash       = getHash src;
      origin_url = buildOriginUrl type { inherit domain owner repo; };
      # Sourcehut archive URL format
      fetch_url  = safeBuildUrl [owner repo rev] (_: "https://git.sr.ht/~${owner}/${repo}/archive/${rev}.tar.gz");
    };

    # ---- fetchgit (generic git) ---------------------------------------
    fetchgit = src: rec {
      type       = "fetchgit";
      url        = tryStr src "url";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = if rev_type == "tag" && rev != null then rev else null;
      hash       = getHash src;
      fetchSubmodules = src.fetchSubmodules or false;
      origin_url = url;
      fetch_url  = null; # no standard archive URL for generic git
    };

    # ---- fetchsvn -----------------------------------------------------
    fetchsvn = src: rec {
      type       = "fetchsvn";
      url        = tryStr src "url";
      # SVN uses integer revision numbers, never tags in the VCS sense
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      tag_name   = null;
      hash       = getHash src;
      origin_url = url;
      fetch_url  = null;
    };

    # ---- fetchhg (Mercurial) -----------------------------------------
    fetchhg = src: rec {
      type       = "fetchhg";
      url        = tryStr src "url";
      rev        = tryStr src "rev";
      rev_type   = revType rev;
      # Mercurial supports named tags — flag them
      tag_name   = if rev_type == "tag" && rev != null then rev else null;
      hash       = getHash src;
      origin_url = url;
      fetch_url  = null;
    };

    # ---- fetchurl -----------------------------------------------------
    fetchurl = src: rec {
      type       = "fetchurl";
      url        = src.url or (
                     let urls = src.urls or [];
                     in if urls != [] then builtins.head urls else null
                   );
      urls       = src.urls or (if src ? url then [ src.url ] else []);
      rev        = null;
      rev_type   = "none";
      tag_name   = null;
      hash       = getHash src;
      origin_url = url;
      fetch_url  = url;
    };

    # ---- fetchzip -----------------------------------------------------
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

    # ---- fetchTarball (builtins) --------------------------------------
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
  # FIX: Classification logic with proper forge detection order
  # Most specific signals first, GitHub assumption only as last resort
  # ------------------------------------------------------------------ #

  classifySrc = src:
    if !builtins.isAttrs src then { type = "non-attrset"; }
    else
      let
        url    = src.url    or "";
        domain = src.domain or "";
      in

      # ── forge fetchers: identified by owner+repo+rev presence ──────
      if src ? owner && src ? repo && src ? rev then
             if lib.hasInfix "github.com"    url                         then extractors.fetchFromGitHub    src
        else if lib.hasInfix "gitlab"        url
             || lib.hasInfix "gitlab"        domain                      then extractors.fetchFromGitLab    src
        else if lib.hasInfix "bitbucket.org" url                         then extractors.fetchFromBitbucket src
        else if lib.hasInfix "sr.ht"         url
             || lib.hasInfix "sr.ht"         domain                      then extractors.fetchFromSourcehut src
        else if lib.hasInfix "codeberg.org"  url
             || lib.hasInfix "codeberg.org"  domain                      then extractors.fetchFromForgejo   src
        else if lib.hasInfix "forgejo"       domain                      then extractors.fetchFromForgejo   src
        else if lib.hasInfix "gitea"         domain
             || lib.hasInfix "gitea"         url                         then extractors.fetchFromGitea     src
        else if lib.hasInfix "gogs"          domain                      then extractors.fetchFromGogs      src
        # A non-empty domain that matched nothing above → likely
        # a self-hosted Gitea/Forgejo; record it but don't assume GitHub
        else if domain != ""                                              then extractors.fetchFromGitea     src
        # FIX: only assume GitHub when there is truly no other signal
        else if domain == "" && url == ""                                 then extractors.fetchFromGitHub    src
        # Has a URL but unknown forge — record as fetchgit
        else                                                              extractors.fetchgit               src

      # ── VCS fetchers identified by other attributes ─────────────────
      else if src ? url && src ? rev && src ? svnRoot                    then extractors.fetchsvn           src
      else if src ? url && src ? rev && src ? hgHash                     then extractors.fetchhg            src
      else if src ? url && src ? rev                                     then extractors.fetchgit           src

      # ── archive fetchers ────────────────────────────────────────────
      else if src ? urls                                                  then extractors.fetchurl           src
      else if src ? url then
        let u = src.url or ""; in
             if lib.hasSuffix ".zip"     u
             || lib.hasSuffix ".tar.gz"  u
             || lib.hasSuffix ".tar.bz2" u
             || lib.hasSuffix ".tar.xz"  u
             || lib.hasSuffix ".tgz"     u                               then extractors.fetchzip           src
        else                                                              extractors.fetchurl               src

      else { type = "unknown"; };

  # ------------------------------------------------------------------ #
  # Safe per-package extraction
  # ------------------------------------------------------------------ #

  safeExtract = attrPath: pkg:
    let
      isCandidateResult = builtins.tryEval (lib.isDerivation pkg && pkg ? src);
      isCandidate       = isCandidateResult.success && isCandidateResult.value;
    in
    if !isCandidate then null
    else
      let
        metaCheck = builtins.tryEval (
          let
            meta   = pkg.meta or {};
            broken = meta.broken or false;
          in
          { available = !broken; }
        );
        shouldSkip = !metaCheck.success
                     || (metaCheck.success && !metaCheck.value.available);
      in
      if shouldSkip then null
      else
        let
          srcResult = builtins.tryEval pkg.src;
        in
        if !srcResult.success || srcResult.value == null then null
        else
          let
            src            = srcResult.value;
            classifyResult = builtins.tryEval (classifySrc src);
            srcInfo        = if classifyResult.success
                             then classifyResult.value
                             else { type = "eval-error"; };
            versionResult  = builtins.tryEval (pkg.version or null);
            pnameResult    = builtins.tryEval (pkg.pname   or null);
          in
          if srcInfo.type == "unknown"
             || srcInfo.type == "non-attrset"
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
  # FIX: removed duplicate python versioned scopes (python3Packages
  #      already aliases one of them); kept only the canonical alias
  #      to avoid double-counting in the research dataset
  # ------------------------------------------------------------------ #

  scopes = [
    { name = "python3Packages";  set = pkgs.python3Packages;  }
    { name = "nodePackages";     set = pkgs.nodePackages;      }
    { name = "perlPackages";     set = pkgs.perlPackages;      }
    { name = "rubyPackages";     set = pkgs.rubyPackages;      }
    { name = "haskellPackages";  set = pkgs.haskellPackages;   }
    { name = "ocamlPackages";    set = pkgs.ocamlPackages;     }
    { name = "rPackages";        set = pkgs.rPackages;         }
    { name = "luaPackages";      set = pkgs.luaPackages;       }
    { name = "phpPackages";      set = pkgs.phpPackages;       }
    { name = "emacsPackages";    set = pkgs.emacsPackages;     }
    { name = "vimPlugins";       set = pkgs.vimPlugins;        }
    { name = "gnome";            set = pkgs.gnome;             }
    { name = "xfce";             set = pkgs.xfce;              }
    { name = "kde";              set = pkgs.kdePackages;       }
    { name = "plasma5Packages";  set = pkgs.plasma5Packages;   }
    # linuxPackages intentionally excluded: fails on non-Linux hosts
  ];

  # Crawl one attribute set, prefix every key with `prefix.`
  crawlScope = prefix: attrset:
    let
      namesResult = builtins.tryEval (builtins.attrNames attrset);
      names       = if namesResult.success then namesResult.value else [];
    in
    lib.foldl'
      (acc: name:
        if isBlacklisted name then acc
        else
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
        if isBlacklisted name then acc
        else
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

  # Scoped crawl
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
