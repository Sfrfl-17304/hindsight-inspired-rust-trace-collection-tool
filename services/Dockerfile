FROM golang:1.23-alpine AS build
WORKDIR /src
COPY go.mod ./
COPY cmd ./cmd
RUN CGO_ENABLED=0 go build -o /out/service ./cmd/service \
 && CGO_ENABLED=0 go build -o /out/loadgen ./cmd/loadgen

FROM alpine:3.20
# iproute2 provides `tc`, used later to inject network faults (netem) per node.
RUN apk add --no-cache iproute2 curl
COPY --from=build /out/ /usr/local/bin/
ENTRYPOINT ["service"]
