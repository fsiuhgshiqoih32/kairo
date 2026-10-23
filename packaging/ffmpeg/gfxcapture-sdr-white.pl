#!/usr/bin/env perl
# Adds an `sdr_white` option to ffmpeg's gfxcapture (Windows Graphics Capture), for Kairo.
#
# On a screen in Windows HDR mode, the desktop is composed in scRGB: linear light where 1.0 is
# 80 nits and ordinary (SDR) content sits at the "SDR content brightness" level, often 200 to
# 480 nits. Asked for 8-bit frames, Windows converts them itself and clips at 80 nits, so
# recordings come out far too bright. With `sdr_white=<nits>`, gfxcapture captures in 16-bit
# float instead and its shader scales by 80/nits before encoding sRGB, so SDR content comes
# out as it looks on screen; brighter HDR highlights roll off softly instead of clipping.
#
#   perl gfxcapture-sdr-white.pl <ffmpeg source dir>
#
# Each change must apply exactly where expected, or this stops with an error. Running it twice
# changes nothing.

use strict;
use warnings;

my $src = shift or die "usage: $0 <ffmpeg source dir>\n";

sub slurp {
    my ($p) = @_;
    open(my $f, '<', $p) or die "$p: $!\n";
    local $/;
    my $s = <$f>;
    close $f;
    return $s;
}

sub spit {
    my ($p, $s) = @_;
    open(my $f, '>', $p) or die "$p: $!\n";
    binmode $f;
    print $f $s;
    close $f;
}

# Replaces what `$re` matches (it must match `$want` times, or at least once if `$want` is 0).
sub change {
    my ($name, $s, $re, $with, $want) = @_;
    my $n = 0;
    $n++ while $$s =~ /$re/g;
    die "$name: expected " . ($want || 'some') . " match(es) of $re, found $n\n"
        if ($want && $n != $want) || (!$want && $n == 0);
    $$s =~ s/$re/$with->()/ge;
}

my $h_path = "$src/libavfilter/vsrc_gfxcapture.h";
my $h = slurp($h_path);
if ($h =~ /sdr_white/) {
    print "gfxcapture already has sdr_white\n";
    exit 0;
}

# The option's field.
change($h_path, \$h, qr/(\n([ \t]*)int premult_alpha;)/,
    sub { "$1\n$2float sdr_white;" }, 1);

# The option.
my $c_path = "$src/libavfilter/vsrc_gfxcapture.c";
my $c = slurp($c_path);
change($c_path, \$c, qr/(\n([ \t]*)\{ "output_fmt",)/,
    sub {
        "\n$2" . '{ "sdr_white",       "SDR white level of an HDR screen in nits: capture in float and map it to SDR (0: Windows converts)",'
          . "\n$2" . '                                                          OFFSET(sdr_white),      AV_OPT_TYPE_FLOAT,      { .dbl = 0 },      0, 10000,      FLAGS },'
          . $1
    }, 1);

# Capture in float when mapping: the frame pool is made, and made again on a size change.
my $w_path = "$src/libavfilter/vsrc_gfxcapture_winrt.cpp";
my $w = slurp($w_path);
change($w_path, \$w, qr/if \(cctx->out_fmt != AV_PIX_FMT_BGRA\)/,
    sub { 'if (cctx->out_fmt != AV_PIX_FMT_BGRA || cctx->sdr_white > 0)' }, 0);

# The shader's gain: 80 / nits, or 1.
change($w_path, \$w, qr/(\n([ \t]*)cb_u\[9\][^;]*;[^\n]*)/,
    sub { "$1\n$2cb_f[10] = cctx->sdr_white > 0 ? 80.0f / cctx->sdr_white : 1.0f; // gain" }, 1);

# The shader: the gain takes the constant buffer's spare slot; scaled light rolls off above 0.9.
my $s_path = "$src/libavfilter/vsrc_gfxcapture_shader.h";
my $s = slurp($s_path);
change($s_path, \$s, qr/uint2\s+pad;/, sub { "float gain;\n        uint pad;" }, 1);
change($s_path, \$s, qr/(\n([ \t]*)float4 fix_color\()/,
    sub {
        "\n$2float3 soft_clip(float3 c) {"
          . "\n$2    float m = max(c.r, max(c.g, c.b));"
          . "\n$2    if (m <= 0.9)"
          . "\n$2        return c;"
          . "\n$2    float n = 0.9 + 0.1 * (1.0 - exp(-(m - 0.9) / 0.1));"
          . "\n$2    return c * (n / m);"
          . "\n$2}"
          . "\n$1"
    }, 1);
change($s_path, \$s, qr/c\.rgb = linear_to_srgb\(c\.rgb\);/,
    sub { 'c.rgb = linear_to_srgb(gain != 1.0 ? soft_clip(c.rgb * gain) : c.rgb);' }, 1);

spit($h_path, $h);
spit($c_path, $c);
spit($w_path, $w);
spit($s_path, $s);
print "gfxcapture: added sdr_white\n";
