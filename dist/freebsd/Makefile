# $FreeBSD$
#
# Anvil — AI coding assistant by Culpur Defense.
# https://github.com/culpur/anvil
#
# This is a skeleton port. Actual submission to the FreeBSD ports tree
# is a separate process (poudriere build, port commit review). For now,
# FreeBSD users can install via the curl installer at culpur.net or by
# downloading the binary from the GitHub release directly.

PORTNAME=       anvil
DISTVERSION=    2.2.13
CATEGORIES=     devel
MASTER_SITES=   https://github.com/culpur/anvil/releases/download/v${DISTVERSION}/
DISTFILES=      anvil-${ARCH:S/amd64/x86_64/:S/arm64/aarch64/}-unknown-freebsd

MAINTAINER=     chatgpt@culpur.net
COMMENT=        AI coding assistant — your providers, your credentials, your data
WWW=            https://culpur.net/anvil

LICENSE=        BSD2CLAUSE

NO_BUILD=       yes
NO_ARCH=        yes
USES=           shebangfix

ONLY_FOR_ARCHS= amd64 arm64
ONLY_FOR_ARCHS_REASON=  upstream binaries only for x86_64 and aarch64

do-install:
        ${INSTALL_PROGRAM} ${DISTDIR}/${DISTFILES} ${STAGEDIR}${PREFIX}/bin/anvil

.include <bsd.port.mk>
