#!/usr/bin/perl
# Writes the website into OUT_DIR: one page per language from index.html and i18n/<code>.json,
# plus sitemap.xml and llms.txt. Called by build.sh, which passes the release's data in the
# environment (VERSION, DATE, MSI_MB, ZIP_MB, FLATPAK_MB, MSI_SHA, ZIP_SHA, FLATPAK_SHA).
#
# In index.html: {{t:key}} is the text as HTML, {{a:key}} the text for an attribute, {{j:key}} the
# text as a JavaScript/JSON string, {{N:1.5}} a number with the language's decimal separator.
# Every language must have every key: a missing one stops the build.
use strict;
use warnings;
use utf8;
use JSON::PP;
use File::Basename qw(dirname);
use File::Path qw(make_path);

my $out = shift or die "usage: build.pl OUT_DIR\n";
my $src = dirname(__FILE__);
my $site = 'https://25dark25.github.io/ObsLite/';
my %env = map { $_ => ($ENV{$_} // die "missing $_\n") } qw(VERSION DATE MSI_MB ZIP_MB FLATPAK_MB MSI_SHA ZIP_SHA FLATPAK_SHA);

# Code, folder, hreflang, Open Graph locale, short label for the menu.
my @langs = (
    ['en', '', 'en', 'en_US', 'EN'],
    ['es', 'es/', 'es', 'es_ES', 'ES'],
    ['de', 'de/', 'de', 'de_DE', 'DE'],
    ['fr', 'fr/', 'fr', 'fr_FR', 'FR'],
    ['it', 'it/', 'it', 'it_IT', 'IT'],
    ['pl', 'pl/', 'pl', 'pl_PL', 'PL'],
    ['ru', 'ru/', 'ru', 'ru_RU', 'RU'],
    ['ja', 'ja/', 'ja', 'ja_JP', 'JA'],
    ['zh-hans', 'zh-hans/', 'zh-Hans', 'zh_CN', '简'],
    ['zh-hant', 'zh-hant/', 'zh-Hant', 'zh_TW', '繁'],
);

sub slurp { my ($f) = @_; open my $h, '<:encoding(UTF-8)', $f or die "$f: $!\n"; local $/; my $s = <$h>; close $h; $s }
sub spew { my ($f, $s) = @_; open my $h, '>:encoding(UTF-8)', $f or die "$f: $!\n"; print $h $s; close $h }

my $template = slurp("$src/index.html");
my %text = map { $_->[0] => decode_json_text(slurp("$src/i18n/$_->[0].json")) } @langs;
sub decode_json_text { JSON::PP->new->relaxed(0)->decode($_[0]) }

# Every key the template and the FAQ use, in every language.
my @keys = ($template =~ /\{\{[taj]:(\w+)\}\}/g);
push @keys, qw(lang_name hint decimal mb), map { ("faq${_}_q", "faq${_}_a") } 1 .. 10;
for my $l (@langs) {
    my @missing = grep { !exists $text{$l->[0]}{$_} } @keys;
    die "i18n/$l->[0].json lacks: @missing\n" if @missing;
}

my $json = JSON::PP->new->canonical->pretty;
sub js { my $s = JSON::PP->new->allow_nonref->encode($_[0]); $s =~ s{</}{<\\/}g; $s }
sub attr { my $s = shift; $s =~ s/&(?!amp;|#)/&amp;/g; $s =~ s/"/&quot;/g; $s =~ s/</&lt;/g; $s =~ s/>/&gt;/g; $s }
sub plain { my $s = shift; $s =~ s/<[^>]+>//g; $s =~ s/&amp;/&/g; $s }
sub num { my ($n, $dec) = @_; $n =~ s/\./$dec/; $n }

my @sitemap;
for my $l (@langs) {
    my ($code, $dir, $hreflang, $locale, $label) = @$l;
    my $t = $text{$code};
    my $root = $dir eq '' ? '' : '../';
    my $href = sub { my $d = shift; my $h = $root . $d; $h eq '' ? './' : $h };
    my $page = $template;

    my $faq_html = join "\n", map {
        my $open = $_ == 1 ? ' open' : '';
        qq(      <details$open>\n        <summary>$t->{"faq${_}_q"}</summary>\n        <div class="a"><p>$t->{"faq${_}_a"}</p></div>\n      </details>)
    } 1 .. 10;
    my $faq_ld = $json->encode({
        '@context' => 'https://schema.org',
        '@type' => 'FAQPage',
        inLanguage => $hreflang,
        mainEntity => [map { {
            '@type' => 'Question',
            name => plain($t->{"faq${_}_q"}),
            acceptedAnswer => { '@type' => 'Answer', text => plain($t->{"faq${_}_a"}) },
        } } 1 .. 10],
    });
    $faq_ld =~ s{</}{<\\/}g;
    chomp $faq_ld;
    my $hreflangs = join "\n", (map { qq(<link rel="alternate" hreflang="$_->[2]" href="$site$_->[1]">) } @langs),
        qq(<link rel="alternate" hreflang="x-default" href="$site">);
    my $links = sub {
        join '', map {
            my $current = $_->[0] eq $code ? ' aria-current="page"' : '';
            qq(<a href="@{[$href->($_->[1])]}" hreflang="$_->[2]" lang="$_->[2]"$current>$text{$_->[0]}{lang_name}</a>)
        } @langs
    };
    my $menu = qq(<details class="lang"><summary aria-label="@{[attr("$t->{language}: $t->{lang_name}")]}"><svg aria-hidden="true"><use href="#i-globe"/></svg>$label</summary><ul>)
        . join('', map { '<li>' . $_ . '</li>' } ($links->() =~ /(<a .*?<\/a>)/g)) . '</ul></details>';
    my %hints = map { ($_->[0] => { url => $href->($_->[1]), lang => $_->[2], text => $text{$_->[0]}{hint} }) } grep { $_->[0] ne $code } @langs;

    $page =~ s/\{\{FAQ_HTML\}\}/$faq_html/;
    $page =~ s/\{\{FAQ_JSONLD\}\}/$faq_ld/;
    $page =~ s/\{\{HREFLANG\}\}/$hreflangs/;
    $page =~ s/\{\{LANG_MENU\}\}/$menu/;
    my $footer_links = join ' ', ($links->() =~ /(<a .*?<\/a>)/g);
    $page =~ s/\{\{LANG_LINKS\}\}/$footer_links/;
    my $hints_js = JSON::PP->new->canonical->encode(\%hints);
    $page =~ s/\{\{LANG_HINTS\}\}/$hints_js/;
    $page =~ s/\{\{t:(\w+)\}\}/$t->{$1}/g;
    $page =~ s/\{\{a:(\w+)\}\}/attr($t->{$1})/ge;
    $page =~ s/\{\{j:(\w+)\}\}/js($t->{$1})/ge;

    my $dec = $t->{decimal};
    my %data = (
        VERSION => $env{VERSION}, DATE => $env{DATE},
        MSI_SIZE_NUM => num($env{MSI_MB}, $dec),
        MB => $t->{mb},
        FILE_SIZE => "$env{MSI_MB} MB",
        # A no-break space, so "3.1" and "MB" never end up on different lines.
        MSI_SIZE => num($env{MSI_MB}, $dec) . "\x{a0}$t->{mb}", ZIP_SIZE => num($env{ZIP_MB}, $dec) . "\x{a0}$t->{mb}",
        FLATPAK_SIZE => num($env{FLATPAK_MB}, $dec) . "\x{a0}$t->{mb}",
        MSI_SHA => $env{MSI_SHA}, ZIP_SHA => $env{ZIP_SHA}, FLATPAK_SHA => $env{FLATPAK_SHA},
        LANG => $hreflang, LANG_KEY => $code, ROOT => $root, CANONICAL => "$site$dir", OG_LOCALE => $locale,
    );
    $page =~ s/\{\{N:(-?[\d.]+)\}\}/num($1, $dec)/ge;
    $page =~ s/\{\{(\w+)\}\}/exists $data{$1} ? $data{$1} : "{{$1}}"/ge;

    # What a visit downloads: the page (styles and script inline) and its icon.
    {
        use bytes;
        my $kb = int((length($page) + 8 + -s "$src/favicon.svg") / 1024) + 1;
        $page =~ s/\{\{PAGE_KB\}\}/$kb/g;
    }
    die "$code: unfilled placeholders: @{[ $page =~ /(\{\{[^}]*\}\})/g ]}\n" if $page =~ /\{\{/;
    make_path("$out/$dir") if $dir ne '';
    spew("$out/${dir}index.html", $page);

    my $alternates = join '', map { qq(\n    <xhtml:link rel="alternate" hreflang="$_->[2]" href="$site$_->[1]"/>) } @langs;
    push @sitemap, qq(  <url>\n    <loc>$site$dir</loc>\n    <lastmod>@{[ build_date() ]}</lastmod>$alternates\n    <xhtml:link rel="alternate" hreflang="x-default" href="$site"/>\n  </url>\n);
}

sub build_date { my @t = gmtime; sprintf '%04d-%02d-%02d', $t[5] + 1900, $t[4] + 1, $t[3] }

spew("$out/sitemap.xml", qq(<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">\n) . join('', @sitemap) . "</urlset>\n");

my $llms = slurp("$src/llms.txt");
my %plain = (VERSION => $env{VERSION}, DATE => $env{DATE}, MSI_SIZE => "$env{MSI_MB} MB", FLATPAK_SIZE => "$env{FLATPAK_MB} MB");
$llms =~ s/\{\{(\w+)\}\}/exists $plain{$1} ? $plain{$1} : die "llms.txt: {{$1}}\n"/ge;
spew("$out/llms.txt", $llms);
print scalar(@langs), " pages\n";
