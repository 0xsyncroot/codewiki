#!/usr/bin/env bash
# .NET agent-savings: measure CodeWiki CLI output bytes vs grep/read baseline bytes
# for 5 representative tasks. Tokens = bytes/4. Pricing: $3.00 / 1M input tokens.
set -u
BIN="${CW:-codewiki}"
ROOT="${BENCH_ROOT:-/tmp/bench}"
ES="$ROOT/eShopOnWeb"
JF="$ROOT/jellyfin"
OUT="$(dirname "$0")/results-dotnet.tsv"
echo -e "task\trepo\tcw_calls\tcw_bytes\tbl_calls\tbl_bytes" > "$OUT"

b() { wc -c | tr -d ' '; }   # bytes from stdin
fb() { local t=0; for f in "$@"; do [ -f "$f" ] && t=$(( t + $(wc -c < "$f") )); done; echo "$t"; }

# Measure against a CLEAN cold index — CodeWiki ranked output (context/query) depends on
# index state, so a fresh init gives the reproducible byte counts published in the report.
rm -rf "$ES/.codewiki" "$JF/.codewiki"
"$BIN" init --path "$ES" >/dev/null 2>&1
"$BIN" init --path "$JF" >/dev/null 2>&1

echo "===================== TASK 1: DI consumers of IBasketService (eShopOnWeb) ====="
cw1a=$($BIN query "IBasketService" --path $ES 2>/dev/null | b)
cw1b=$($BIN callers IBasketService --path $ES 2>/dev/null | b)
cw1=$(( cw1a + cw1b ))
echo "CW: query=$cw1a callers=$cw1b total=$cw1 (2 calls)"
g1=$(grep -rn "IBasketService" $ES/src/ 2>/dev/null | b)
# files an agent needs to read to answer (interface + DI reg + 3 consumers)
f1="$ES/src/ApplicationCore/Interfaces/IBasketService.cs
$ES/src/Web/Configuration/ConfigureCoreServices.cs
$ES/src/Web/Pages/Basket/Checkout.cshtml.cs
$ES/src/Web/Pages/Basket/Index.cshtml.cs
$ES/src/Web/Areas/Identity/Pages/Account/Login.cshtml.cs"
fb1=$(fb $f1)
bl1=$(( g1 + fb1 ))
nf1=$(echo "$f1" | grep -c .)
echo "BL: grep=$g1 + $nf1 files=$fb1 total=$bl1 ($((1+nf1)) calls)"
echo -e "T1_DI_consumers\teShopOnWeb\t2\t$cw1\t$((1+nf1))\t$bl1" >> "$OUT"

echo "===================== TASK 2: Feature comprehension basket checkout (eShopOnWeb) ====="
cw2=$($BIN context "how does basket checkout work" --path $ES 2>/dev/null | b)
echo "CW: context=$cw2 (1 call)"
g2=$(grep -rnE "checkout|Checkout|BasketService|IBasketService" $ES/src/ 2>/dev/null | b)
f2="$ES/src/Web/Pages/Basket/Checkout.cshtml.cs
$ES/src/ApplicationCore/Services/BasketService.cs
$ES/src/ApplicationCore/Services/OrderService.cs
$ES/src/ApplicationCore/Interfaces/IBasketService.cs
$ES/src/Web/Pages/Basket/Index.cshtml.cs"
fb2=$(fb $f2); nf2=$(echo "$f2" | grep -c .)
bl2=$(( g2 + fb2 ))
echo "BL: grep=$g2 + $nf2 files=$fb2 total=$bl2 ($((1+nf2)) calls)"
echo -e "T2_feature_comprehension\teShopOnWeb\t1\t$cw2\t$((1+nf2))\t$bl2" >> "$OUT"

echo "===================== TASK 3: Interface->impls IRepository (eShopOnWeb) ====="
cw3a=$($BIN query "IRepository" --path $ES 2>/dev/null | b)
cw3b=$($BIN impact IRepository --path $ES 2>/dev/null | b)
cw3=$(( cw3a + cw3b ))
echo "CW: query=$cw3a impact=$cw3b total=$cw3 (2 calls)"
g3=$(grep -rnE "IRepository|IReadRepository|EfRepository|: IRepository" $ES/src/ 2>/dev/null | b)
f3="$ES/src/ApplicationCore/Interfaces/IRepository.cs
$ES/src/Infrastructure/Data/EfRepository.cs
$ES/src/ApplicationCore/Services/OrderService.cs
$ES/src/Web/Configuration/ConfigureCoreServices.cs
$ES/src/Web/Services/CatalogViewModelService.cs"
fb3=$(fb $f3); nf3=$(echo "$f3" | grep -c .)
bl3=$(( g3 + fb3 ))
echo "BL: grep=$g3 + $nf3 files=$fb3 total=$bl3 ($((1+nf3)) calls)"
echo -e "T3_interface_impls\teShopOnWeb\t2\t$cw3\t$((1+nf3))\t$bl3" >> "$OUT"

echo "===================== TASK 4: Blast radius OrderService (eShopOnWeb) ====="
cw4=$($BIN impact OrderService --path $ES 2>/dev/null | b)
echo "CW: impact=$cw4 (1 call)"
g4=$(grep -rnE "OrderService|IOrderService" $ES/src/ 2>/dev/null | b)
f4="$ES/src/ApplicationCore/Services/OrderService.cs
$ES/src/ApplicationCore/Interfaces/IOrderService.cs
$ES/src/Web/Configuration/ConfigureCoreServices.cs
$ES/src/Web/Pages/Basket/Checkout.cshtml.cs"
fb4=$(fb $f4); nf4=$(echo "$f4" | grep -c .)
bl4=$(( g4 + fb4 ))
echo "BL: grep=$g4 + $nf4 files=$fb4 total=$bl4 ($((1+nf4)) calls)"
echo -e "T4_blast_radius\teShopOnWeb\t1\t$cw4\t$((1+nf4))\t$bl4" >> "$OUT"

echo "===================== TASK 5: Cross-cutting auth config (jellyfin 2065 .cs) ====="
cw5a=$($BIN context "where is authentication configured" --path $JF 2>/dev/null | b)
cw5b=$($BIN context "authentication handler JwtBearer UseAuthentication" --path $JF 2>/dev/null | b)
cw5=$(( cw5a + cw5b ))
echo "CW: context1=$cw5a context2=$cw5b total=$cw5 (2 calls)"
g5a=$(grep -rnE "AuthenticationHandler|AddAuthentication|UseAuthentication|JwtBearer" $JF/Jellyfin.Server/ 2>/dev/null | b)
g5b=$(grep -rnE "JwtBearer|AddAuthentication|UseAuthentication|IAuthorizationHandler" $JF/ 2>/dev/null | grep ".cs:" | head -60 | b)
f5=$(find $JF -name ApiServiceCollectionExtensions.cs -o -name Startup.cs 2>/dev/null | grep "Jellyfin.Server")
fb5=$(fb $f5); nf5=$(echo "$f5" | grep -c .)
bl5=$(( g5a + g5b + fb5 ))
echo "BL: grep1=$g5a grep2=$g5b + $nf5 files=$fb5 total=$bl5 ($((2+nf5)) calls)"
echo -e "T5_auth_config\tjellyfin\t2\t$cw5\t$((2+nf5))\t$bl5" >> "$OUT"

echo ""
echo "=== results-dotnet.tsv ==="
column -t "$OUT"
echo ""
echo "=== savings computation (tokens=bytes/4, \$3/Mtok) ==="
awk -F'\t' 'NR>1{
  cwt=$4/4; blt=$6/4; saved=(blt-cwt)*3/1000000;
  callred=100*($5-$3)/$5; tokred=100*(blt-cwt)/blt;
  printf "%-26s CW:%dc/%dt BL:%dc/%dt  callred=%.0f%% tokred=%.0f%% $%.4f\n",$1,$3,cwt,$5,blt,callred,tokred,saved;
  scw+=$3; sbl+=$5; scwt+=cwt; sblt+=blt; n++
}
END{
  printf "\nAVG  CW calls=%.1f BL calls=%.1f  callred=%.0f%%  CW tok=%.0f BL tok=%.0f tokred=%.0f%%  $/task=%.4f\n",
    scw/n, sbl/n, 100*(sbl-scw)/sbl, scwt/n, sblt/n, 100*(sblt-scwt)/sblt, ((sblt-scwt)/n)*3/1000000;
  printf "TOTAL CW=%d BL=%d calls  CW=%.0f BL=%.0f tok  $total=%.4f  (x20 session=$%.2f)\n",
    scw, sbl, scwt, sblt, (sblt-scwt)*3/1000000, ((sblt-scwt)/n)*3/1000000*20;
}' "$OUT"
