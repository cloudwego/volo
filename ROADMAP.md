# Volo Roadmap

This document provides description of items that the project decided to prioritize. This should
serve as a reference point for Volo contributors to understand where the project is going, and
help determine if a contribution could be conflicting with some longer term plans.

The fact that a feature isn't listed here doesn't mean that a patch for it will automatically be
refused! We are always happy to receive patches for new cool features we haven't thought about,
or didn't judge to be a priority. Please however understand that such patches might take longer
for us to review.

## Service Governance

- [ ] Support OpenSergo

### Service Discovery

- [ ] Support `etcd` as a service discovery provider
- [ ] Support `zookeeper` as a service discovery provider
- [x] Support `nacos` as a service discovery provider
- [ ] Support `polaris` as a service discovery provider
- [ ] Support `eureka` as a service discovery provider
- [ ] Support `consul` as a service discovery provider

### Load Balancer

- [x] #7 Support consistent hash load balancing

### Proxyless

- [ ] Support Proxyless mode

## TLS

- [x] #6 Support TLS for `volo-grpc`

## Cli

- [x] #5 Support auto generate service code in lib.rs
