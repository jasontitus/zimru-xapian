FROM alpine:3.21@sha256:bc21487101ae47172509130ee1092cc273a831968d64b617776d1f69c81e42ab
COPY recipe/alpine-packages.txt /build.packages
RUN xargs apk add --no-cache < /build.packages
COPY type2-runtime-fedfad51.tar.gz fuse-3.15.0.tar.xz squashfuse-0.5.2.tar.gz /opt/inputs/
RUN cd /opt/inputs && printf '%s\n' \
    'e27ea3c837e9c660e1a9a2993b673ae93bbb53077330efba8654031e302d4e8e  type2-runtime-fedfad51.tar.gz' \
    '70589cfd5e1cff7ccd6ac91c86c01be340b227285c5e200baa284e401eea2ca0  fuse-3.15.0.tar.xz' \
    'db0238c5981dabbd80ee09ae15387f390091668ca060a7bc38047912491443d3  squashfuse-0.5.2.tar.gz' | sha256sum -c -
RUN mkdir /upstream && tar xzf /opt/inputs/type2-runtime-fedfad51.tar.gz -C /upstream --strip-components=1 && \
    sed -i -e '/^wget .*libfuse/c\cp /opt/inputs/fuse-3.15.0.tar.xz .' \
           -e '/^wget .*squashfuse/c\cp /opt/inputs/squashfuse-0.5.2.tar.gz 0.5.2.tar.gz' \
           /upstream/scripts/common/install-dependencies.sh && \
    cd /upstream && bash scripts/common/install-dependencies.sh
WORKDIR /upstream
COPY private-extraction.patch /runtime.patch
RUN patch -p1 < /runtime.patch && printf '%s\n' fedfad51-kiwix-private1 > src/runtime/version && \
    scripts/build-runtime.sh
