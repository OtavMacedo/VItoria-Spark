#!/usr/bin/env bash
# VItoria Spark no Linux: o victory.exe roda sob Wine ou Proton com vkd3d-proton (docs/linux.md).
#
# Uso:
#   scripts/linux/spark.sh preparar                  confere dependências e cria o prefixo dedicado
#   scripts/linux/spark.sh compilar [release|diag]   compila o .exe do Windows (cargo-xwin); sem argumento, os dois
#   scripts/linux/spark.sh rodar <pasta-ou-exe> [VAR=valor ...]
#   scripts/linux/spark.sh diagnostico               versões do ambiente, para colar numa issue
#   scripts/linux/spark.sh victory <args ...>        qualquer comando do victory.exe (usado pelo AutoPilot)
#   scripts/linux/spark.sh parar                     encerra tudo o que roda no prefixo dedicado
#
# Variáveis: SPARK_PROTON (pasta de um Proton, em vez do Wine do sistema), SPARK_WINE (binário do wine),
# SPARK_WINEPREFIX (padrão: $XDG_DATA_HOME/vitoria-spark/prefix), VICTORY_EXE (o victory.exe), SPARK_VKD3D_DIR e SPARK_DXVK_DIR (pastas
# extraídas das versões oficiais, para o `preparar` instalar no prefixo), SPARK_SEM_SOM=1 (roda sem driver de áudio).
set -euo pipefail

RAIZ=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
ALVO=x86_64-pc-windows-msvc
DADOS=${XDG_DATA_HOME:-$HOME/.local/share}/vitoria-spark
ESTADO=${XDG_STATE_HOME:-$HOME/.local/state}/vitoria-spark
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/vitoria-spark
export WINEPREFIX=${SPARK_WINEPREFIX:-$DADOS/prefix}
# 64 bits só: o victory.exe e o título são x86-64.
export WINEARCH=win64

WINE_MIN=10.0
VKD3D_MIN=3

erro() { printf 'erro: %s\n' "$*" >&2; exit 1; }
aviso() { printf 'aviso: %s\n' "$*" >&2; }

# O Wine do Proton fica em files/bin (versões atuais) ou dist/bin (antigas).
achar_wine() {
    if [[ -n ${SPARK_WINE:-} ]]; then
        WINE=$SPARK_WINE
    elif [[ -n ${SPARK_PROTON:-} ]]; then
        WINE=
        for d in "$SPARK_PROTON/files/bin" "$SPARK_PROTON/dist/bin"; do
            [[ -x $d/wine ]] && WINE=$d/wine && break
        done
        [[ -n $WINE ]] || erro "não achei bin/wine dentro de SPARK_PROTON=$SPARK_PROTON"
    elif command -v wine >/dev/null 2>&1; then
        WINE=wine
    elif command -v wine64 >/dev/null 2>&1; then
        WINE=wine64
    else
        WINE=
        return 1
    fi
    local dir
    dir=$(dirname "$(command -v "$WINE")")
    if [[ -x $dir/wineserver ]]; then
        WINESERVER=$dir/wineserver
    else
        WINESERVER=wineserver
    fi
}

versao_wine() { "$WINE" --version 2>/dev/null | head -n1 | grep -oE '[0-9]+\.[0-9]+' | head -n1 || true; }

# Verdadeiro se a versão $1 >= $2 (só major.minor).
versao_ok() {
    local a=${1%%.*} b=${1#*.} c=${2%%.*} d=${2#*.}
    (( a > c || (a == c && ${b%%.*} >= ${d%%.*}) ))
}

SYS32() { printf '%s\n' "$WINEPREFIX/drive_c/windows/system32"; }

# DLLs embutidas do Wine levam esta marca no cabeçalho; as do vkd3d-proton e do DXVK não.
eh_nativa() { [[ -f $1 ]] && ! grep -qaE 'Wine (builtin|placeholder) DLL' "$1"; }

versao_vkd3d() {
    grep -aoE 'vkd3d-proton[ -]v?[0-9]+\.[0-9]+(\.[0-9]+)?' "$(SYS32)/d3d12core.dll" 2>/dev/null \
        | head -n1 | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' || true
}

familia_distro() {
    local id=""
    if [[ -r /etc/os-release ]]; then
        # shellcheck disable=SC1091
        id=$(. /etc/os-release; printf '%s %s' "${ID:-}" "${ID_LIKE:-}")
    fi
    case " $id " in
        *" debian "*|*" ubuntu "*) echo debian ;;
        *" fedora "*|*" rhel "*) echo fedora ;;
        *" arch "*) echo arch ;;
        *) echo outra ;;
    esac
}

FALTAS=0
# falta <o que> <debian> <fedora> <arch>: imprime o comando da distro detectada (ou os três).
falta() {
    FALTAS=$((FALTAS + 1))
    printf '  FALTA: %s\n' "$1"
    local fam
    fam=$(familia_distro)
    [[ $fam == debian || $fam == outra ]] && printf '    Debian/Ubuntu: %s\n' "$2"
    [[ $fam == fedora || $fam == outra ]] && printf '    Fedora:        %s\n' "$3"
    [[ $fam == arch || $fam == outra ]] && printf '    Arch:          %s\n' "$4"
    return 0
}

ok() { printf '  ok: %s\n' "$*"; }

# Copia d3d12/d3d12core (vkd3d-proton) e dxgi (DXVK, que o vkd3d-proton usa para a swapchain) para o prefixo.
instalar_dlls() {
    local vkd3d=${SPARK_VKD3D_DIR:-} dxvk=${SPARK_DXVK_DIR:-} f origem
    if [[ -z $vkd3d && -n ${SPARK_PROTON:-} ]]; then
        f=$(find "$SPARK_PROTON" -path '*vkd3d*' -name d3d12core.dll 2>/dev/null | grep -E 'x86_64|x64|lib64' | head -n1 || true)
        [[ -n $f ]] && vkd3d=$(dirname "$f")
    fi
    if [[ -z $dxvk && -n ${SPARK_PROTON:-} ]]; then
        f=$(find "$SPARK_PROTON" -path '*dxvk*' -name dxgi.dll 2>/dev/null | grep -E 'x86_64|x64|lib64' | head -n1 || true)
        [[ -n $f ]] && dxvk=$(dirname "$f")
    fi
    for origem in "$vkd3d" "$vkd3d/x64"; do
        if [[ -n $vkd3d && -f $origem/d3d12.dll && -f $origem/d3d12core.dll ]]; then
            cp -f "$origem/d3d12.dll" "$origem/d3d12core.dll" "$(SYS32)/"
            ok "vkd3d-proton copiado de ${origem/#$HOME/<HOME>}"
            break
        fi
    done
    for origem in "$dxvk" "$dxvk/x64"; do
        if [[ -n $dxvk && -f $origem/dxgi.dll ]]; then
            cp -f "$origem/dxgi.dll" "$(SYS32)/"
            ok "dxgi do DXVK copiado de ${origem/#$HOME/<HOME>}"
            break
        fi
    done
}

cmd_preparar() {
    echo "== Wine"
    if achar_wine; then
        local v
        v=$(versao_wine)
        if [[ -z $v ]]; then
            aviso "não consegui ler a versão de $WINE"
        elif versao_ok "$v" "$WINE_MIN"; then
            ok "wine $v"
        else
            falta "wine $WINE_MIN ou mais novo (encontrado: $v), ou um Proton em SPARK_PROTON" \
                "sudo apt install wine   (versões novas: repositório do WineHQ)" \
                "sudo dnf install wine" \
                "sudo pacman -S wine   (repositório multilib)"
        fi
    else
        falta "wine (ou um Proton em SPARK_PROTON=<pasta do Proton>)" \
            "sudo apt install wine   (versões novas: repositório do WineHQ)" \
            "sudo dnf install wine" \
            "sudo pacman -S wine   (repositório multilib)"
    fi

    echo "== Vulkan"
    if command -v vulkaninfo >/dev/null 2>&1; then
        local info
        info=$(vulkaninfo 2>/dev/null || true)
        if grep -q VK_EXT_mesh_shader <<<"$info"; then
            ok "driver Vulkan com VK_EXT_mesh_shader"
        elif [[ -z $info ]]; then
            falta "um driver Vulkan funcionando (vulkaninfo não listou nenhuma placa)" \
                "sudo apt install mesa-vulkan-drivers   (NVIDIA: o driver proprietário da distro)" \
                "sudo dnf install mesa-vulkan-drivers   (NVIDIA: o driver proprietário, RPM Fusion)" \
                "sudo pacman -S vulkan-radeon   (Intel: vulkan-intel; NVIDIA: nvidia-utils)"
        else
            falta "placa ou driver com VK_EXT_mesh_shader (NVIDIA RTX 20+, AMD RX 6000+ com RADV recente)" \
                "atualize o Mesa (mesa-vulkan-drivers) ou o driver NVIDIA" \
                "atualize o Mesa (mesa-vulkan-drivers) ou o driver NVIDIA" \
                "atualize vulkan-radeon / vulkan-intel ou nvidia-utils"
        fi
    else
        falta "vulkaninfo (para conferir mesh shaders)" \
            "sudo apt install vulkan-tools" "sudo dnf install vulkan-tools" "sudo pacman -S vulkan-tools"
    fi

    echo "== Prefixo ($( printf '%s' "$WINEPREFIX" | sed "s#^$HOME#<HOME>#" ))"
    if [[ -n ${WINE:-} ]]; then
        if [[ ! -f $WINEPREFIX/system.reg ]]; then
            mkdir -p "$WINEPREFIX"
            # mscoree/mshtml vazios: o wineboot não pede para instalar Mono e Gecko, que o Spark não usa.
            WINEDEBUG=-all WINEDLLOVERRIDES="mscoree,mshtml=" "$WINE" wineboot -u >/dev/null 2>&1 \
                || erro "o wineboot falhou ao criar o prefixo"
            "$WINESERVER" -w || true
            ok "prefixo criado"
        else
            ok "prefixo existe"
        fi
        instalar_dlls
        if eh_nativa "$(SYS32)/d3d12.dll" && eh_nativa "$(SYS32)/d3d12core.dll"; then
            local vv
            vv=$(versao_vkd3d)
            if [[ -z $vv ]]; then
                ok "vkd3d-proton no prefixo (versão não identificada; $VKD3D_MIN.x ou mais novo recomendado)"
            elif (( ${vv%%.*} >= VKD3D_MIN )); then
                ok "vkd3d-proton $vv no prefixo"
            else
                aviso "vkd3d-proton $vv no prefixo; recomendado $VKD3D_MIN.x ou mais novo"
            fi
        else
            falta "vkd3d-proton no prefixo (d3d12.dll e d3d12core.dll)" \
                "baixe a versão em github.com/HansKristian-Work/vkd3d-proton/releases, extraia e rode: SPARK_VKD3D_DIR=<pasta> $0 preparar" \
                "o mesmo (ou SPARK_PROTON=<pasta do Proton>)" \
                "o mesmo, ou o pacote vkd3d-proton-bin do AUR com SPARK_VKD3D_DIR=/usr/share/vkd3d-proton"
        fi
        if eh_nativa "$(SYS32)/dxgi.dll"; then
            ok "dxgi do DXVK no prefixo"
        else
            falta "dxgi.dll do DXVK no prefixo (o vkd3d-proton cria a swapchain por ele)" \
                "baixe a versão em github.com/doitsujin/dxvk/releases, extraia e rode: SPARK_DXVK_DIR=<pasta> $0 preparar" \
                "o mesmo (ou SPARK_PROTON=<pasta do Proton>)" \
                "o mesmo, ou o pacote dxvk-bin do AUR"
        fi
    fi

    echo "== Sistema"
    local mapas
    mapas=$(cat /proc/sys/vm/max_map_count 2>/dev/null || echo 0)
    if (( mapas >= 1048576 )); then
        ok "vm.max_map_count = $mapas"
    else
        aviso "vm.max_map_count = $mapas; jogos pesados sob Wine costumam precisar de 1048576:"
        printf '    sudo sysctl -w vm.max_map_count=1048576   (permanente: um arquivo em /etc/sysctl.d)\n'
    fi

    local faltas_rodar=$FALTAS
    echo "== Compilar (opcional: só se for compilar aqui)"
    if command -v cargo >/dev/null 2>&1; then ok "cargo"; else
        falta "rustup e cargo" "sudo apt install rustup" "sudo dnf install rustup" "sudo pacman -S rustup"
    fi
    if command -v cargo-xwin >/dev/null 2>&1; then ok "cargo-xwin"; else
        printf '  FALTA: cargo-xwin\n    todas: cargo install --locked cargo-xwin\n'
        FALTAS=$((FALTAS + 1))
    fi
    if command -v clang >/dev/null 2>&1 && command -v lld-link >/dev/null 2>&1 && command -v llvm-lib >/dev/null 2>&1; then
        ok "clang, lld e llvm"
    else
        falta "clang, lld e llvm (o cargo-xwin usa clang-cl, lld-link e llvm-lib)" \
            "sudo apt install clang lld llvm" "sudo dnf install clang lld llvm" "sudo pacman -S clang lld llvm"
    fi

    echo
    if (( FALTAS == 0 )); then
        echo "Tudo pronto."
    else
        echo "$FALTAS item(ns) faltando. Os itens de \"Compilar\" só importam se for compilar nesta máquina."
    fi
    (( faltas_rodar == 0 ))
}

cmd_compilar() {
    local qual=${1:-ambos}
    command -v cargo >/dev/null 2>&1 || erro "falta o cargo (rode: $0 preparar)"
    command -v cargo-xwin >/dev/null 2>&1 || erro "falta o cargo-xwin: cargo install --locked cargo-xwin"
    cd "$RAIZ"
    # O cargo-xwin baixa o CRT e o SDK do Windows (licença da Microsoft) na primeira vez.
    case $qual in
        release|ambos) cargo xwin build --release -p victory --target "$ALVO" ;;&
        diag|ambos) cargo xwin build --profile diag -p victory --features diagnostics --target "$ALVO" ;;
        release) ;;
        *) erro "use: compilar [release|diag]" ;;
    esac
    echo "pronto: target/$ALVO/<release|diag>/victory.exe"
}

achar_exe() {
    if [[ -n ${VICTORY_EXE:-} ]]; then
        [[ -f $VICTORY_EXE ]] || erro "VICTORY_EXE não existe: $VICTORY_EXE"
        printf '%s\n' "$VICTORY_EXE"
        return
    fi
    local c
    for c in "$RAIZ/target/$ALVO/release/victory.exe" "$RAIZ/target/release/victory.exe"; do
        [[ -f $c ]] && printf '%s\n' "$c" && return
    done
    erro "não achei o victory.exe: rode \"$0 compilar\" ou defina VICTORY_EXE"
}

# O executável do título: o .exe dado, o único Executable de MicrosoftGame.config ou o único .exe da pasta.
achar_titulo() {
    local alvo=$1
    [[ -f $alvo ]] && { printf '%s\n' "$alvo"; return; }
    [[ -d $alvo ]] || erro "não existe: $alvo"
    local nomes exes
    if [[ -f $alvo/MicrosoftGame.config ]]; then
        mapfile -t nomes < <(grep -oE '<Executable[^>]*Name="[^"]+"' "$alvo/MicrosoftGame.config" | sed -E 's/.*Name="([^"]+)".*/\1/')
        if (( ${#nomes[@]} == 1 )); then
            printf '%s\n' "$alvo/${nomes[0]//\\//}"
            return
        fi
    fi
    mapfile -t exes < <(find "$alvo" -maxdepth 1 -iname '*.exe')
    (( ${#exes[@]} == 1 )) || erro "não deu para escolher o executável: passe o .exe"
    printf '%s\n' "${exes[0]}"
}

para_windows() {
    local p
    p=$(WINEDEBUG=-all "$WINE" winepath.exe -w "$1" 2>/dev/null | tr -d '\r' | head -n1 || true)
    [[ -n $p ]] || erro "o winepath não converteu: $1"
    printf '%s\n' "$p"
}

# Prepara o ambiente do processo: overrides do vkd3d-proton, logs e caches fora do repositório, caminhos no formato
# do Windows. Os argumentos VAR=valor viram variáveis.
ambiente() {
    achar_wine || erro "falta o wine (rode: $0 preparar)"
    [[ -f $WINEPREFIX/system.reg ]] || erro "prefixo não criado: rode \"$0 preparar\""
    local par
    for par in "$@"; do
        [[ $par == *=* ]] || erro "esperava VAR=valor: $par"
        export "${par%%=*}=${par#*=}"
    done
    local nossos="d3d12,d3d12core,dxgi=n,b;winemenubuilder.exe=d"
    export WINEDLLOVERRIDES="$nossos${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"
    export WINEDEBUG=${WINEDEBUG:--all}
    # O DXVK grava o log do dxgi na pasta atual por padrão.
    export DXVK_LOG_LEVEL=${DXVK_LOG_LEVEL:-none}
    # O vkd3d-proton grava o cache de pipelines na pasta atual por padrão.
    mkdir -p "$CACHE/vkd3d"
    export VKD3D_SHADER_CACHE_PATH=${VKD3D_SHADER_CACHE_PATH:-$CACHE/vkd3d}
    if [[ -z ${VICTORY_CONFIG:-} && -f $RAIZ/victory.toml ]]; then
        export VICTORY_CONFIG=$RAIZ/victory.toml
    fi
    # O Spark é um programa do Windows: caminhos absolutos do Linux nas suas variáveis viram Z:\...
    local var
    for var in $(compgen -e); do
        case $var in
            VICTORY_*|SPARK_LOG_DIR|VKD3D_SHADER_CACHE_PATH)
                [[ ${!var} == /* ]] && export "$var=$(para_windows "${!var}")"
                ;;
        esac
    done
    mkdir -p "$ESTADO"
    # Fora do repositório, para que nenhum arquivo solto caia nele.
    cd "$ESTADO"
}

# Roda o victory.exe sob o Wine. Com SPARK_SEM_SOM=1, o prefixo fica sem driver de áudio durante a execução.
executar() {
    local exe=$1
    shift
    if [[ ${SPARK_SEM_SOM:-0} == 1 ]]; then
        WINEDEBUG=-all "$WINE" reg add 'HKCU\Software\Wine\Drivers' /v Audio /t REG_SZ /d '' /f >/dev/null 2>&1 \
            || aviso "não consegui desligar o áudio do prefixo"
        trap 'WINEDEBUG=-all "$WINE" reg delete "HKCU\\Software\\Wine\\Drivers" /v Audio /f >/dev/null 2>&1 || true' EXIT
    fi
    local codigo=0
    "$WINE" "$exe" "$@" || codigo=$?
    return "$codigo"
}

cmd_rodar() {
    (( $# >= 1 )) || erro "use: rodar <pasta-ou-exe-do-título> [VAR=valor ...]"
    local titulo exe
    titulo=$(achar_titulo "$1")
    titulo=$(cd "$(dirname "$titulo")" && pwd)/$(basename "$titulo")
    shift
    exe=$(achar_exe)
    export VICTORY_GAME_DIR=${VICTORY_GAME_DIR:-$(dirname "$titulo")}
    export SPARK_LOG_DIR=${SPARK_LOG_DIR:-$ESTADO/execucoes/$(date +%Y-%m-%d_%H-%M-%S)}
    mkdir -p "$ESTADO/execucoes"
    [[ $SPARK_LOG_DIR == /* ]] && mkdir -p "$SPARK_LOG_DIR" && echo "logs: ${SPARK_LOG_DIR/#$HOME/<HOME>}"
    ambiente "$@"
    executar "$exe" run "$(para_windows "$titulo")" --execute
}

cmd_victory() {
    local exe args=() a
    exe=$(achar_exe)
    # Caminhos relativos que existem viram absolutos antes de o ambiente mudar de pasta.
    for a in "$@"; do
        if [[ $a != /* && -e $a ]]; then args+=("$PWD/$a"); else args+=("$a"); fi
    done
    set -- "${args[@]}"
    args=()
    ambiente
    for a in "$@"; do
        if [[ $a == /* ]]; then args+=("$(para_windows "$a")"); else args+=("$a"); fi
    done
    executar "$exe" "${args[@]}"
}

cmd_parar() {
    achar_wine || erro "falta o wine"
    "$WINESERVER" -k 2>/dev/null || true
}

cmd_diagnostico() {
    {
        echo "VItoria Spark: $(git -C "$RAIZ" rev-parse --short=10 HEAD 2>/dev/null || echo desconhecido)"
        # shellcheck disable=SC1091
        echo "Distro: $( (. /etc/os-release 2>/dev/null; echo "${PRETTY_NAME:-desconhecida}") )"
        echo "Kernel: $(uname -r)"
        echo "Sessão: ${XDG_SESSION_TYPE:-desconhecida}"
        echo "CPU: $(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ *//')"
        echo "RAM: $(awk '/MemTotal/ {printf "%.0f GB", $2/1048576}' /proc/meminfo 2>/dev/null)"
        echo "vm.max_map_count: $(cat /proc/sys/vm/max_map_count 2>/dev/null || echo ?)"
        if command -v lspci >/dev/null 2>&1; then
            lspci 2>/dev/null | grep -E 'VGA|3D|Display' | sed 's/^[^ ]* /GPU: /' || true
        fi
        if command -v nvidia-smi >/dev/null 2>&1; then
            echo "NVIDIA: $(nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>/dev/null | head -n1)"
        fi
        if command -v vulkaninfo >/dev/null 2>&1; then
            vulkaninfo --summary 2>/dev/null | grep -E '^\s*(apiVersion|driverName|driverInfo|deviceName)\s*=' \
                | sed -E 's/^\s+/Vulkan /; s/\s+=\s+/: /' || true
            if vulkaninfo 2>/dev/null | grep -q VK_EXT_mesh_shader; then
                echo "VK_EXT_mesh_shader: sim"
            else
                echo "VK_EXT_mesh_shader: não"
            fi
        else
            echo "Vulkan: vulkaninfo ausente"
        fi
        if achar_wine; then
            echo "Wine: $("$WINE" --version 2>/dev/null | head -n1)"
            [[ -n ${SPARK_PROTON:-} ]] && echo "Proton: $(basename "$SPARK_PROTON")"
        else
            echo "Wine: ausente"
        fi
        if eh_nativa "$(SYS32)/d3d12core.dll"; then
            echo "vkd3d-proton: $(versao_vkd3d | grep . || echo 'presente, versão não identificada')"
        else
            echo "vkd3d-proton: ausente no prefixo"
        fi
        if eh_nativa "$(SYS32)/dxgi.dll"; then echo "dxgi do DXVK: presente"; else echo "dxgi do DXVK: ausente"; fi
    } | sed "s#$HOME#<HOME>#g"
}

uso() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; }

case ${1:-} in
    preparar) shift; cmd_preparar "$@" ;;
    compilar) shift; cmd_compilar "$@" ;;
    rodar) shift; cmd_rodar "$@" ;;
    diagnostico|diagnóstico) shift; cmd_diagnostico "$@" ;;
    victory) shift; cmd_victory "$@" ;;
    parar) shift; cmd_parar "$@" ;;
    *) uso; exit 2 ;;
esac
