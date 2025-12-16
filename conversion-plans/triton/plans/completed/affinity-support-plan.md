# Triton CLI Affinity Rules Support Plan

## Status: COMPLETED (2025-12-16)

## Background

The `triton instance create` command previously had an `--affinity` flag that accepted affinity rules, but the rules were not passed to the CloudAPI. This feature has now been implemented.

## CloudAPI Context

### Historical Context

CloudAPI has two mechanisms for instance placement hints:

1. **`locality`** (Deprecated in v8.3.0): Object-based format
   ```json
   {
     "strict": false,
     "near": ["uuid1", "uuid2"],
     "far": ["uuid3"]
   }
   ```

2. **`affinity`** (Added in v8.3.0, preferred): Array of rule strings
   ```
   ["instance==silent_bob", "role!=database", "instance!=~foo*"]
   ```

### Affinity Rule Format

Rules follow the pattern: `<key><operator><value>`

**Keys:**
- `instance` or `container`: Match against instance names or UUIDs
- `<tagName>`: Match against tag values (e.g., `role`, `env`)

**Operators:**
- `==`: Must be on same server (strict)
- `!=`: Must be on different server (strict)
- `==~`: Should be on same server (soft/hint)
- `!=~`: Should be on different server (soft/hint)

**Values:**
- Exact string: `instance==myvm`
- Glob pattern: `instance!=foo*`
- Regex: `instance!=/^foo/`

### Examples

```bash
# Run on same node as instance "silent_bob"
triton instance create -a instance==silent_bob ...

# Run on different node from instances tagged role=database
triton instance create -a 'role!=database' ...

# Run on different node from instances starting with "foo" (soft rule)
triton instance create -a 'instance!=~foo*' ...
```

## Implementation Summary

### Changes Made

1. **API Types** (`apis/cloudapi-api/src/types/machine.rs`)
   - Added `affinity: Option<Vec<String>>` field to `CreateMachineRequest`
   - Added comprehensive documentation for the field
   - Updated `locality` field documentation to note deprecation

2. **OpenAPI Spec** (`openapi-specs/generated/cloudapi-api.json`)
   - Regenerated to include the new `affinity` field

3. **CLI** (`cli/triton-cli/src/commands/instance/create.rs`)
   - Wired `--affinity` flag to the API request builder
   - Enhanced help text with format documentation and examples
   - Removed TODO comment about unsupported affinity

### Usage

```bash
# Run on same node as instance "silent_bob"
triton instance create --affinity instance==silent_bob myimage mypackage

# Run on different node from instances tagged role=database
triton instance create --affinity 'role!=database' myimage mypackage

# Run on different node from instances starting with "foo" (soft rule)
triton instance create --affinity 'instance!=~foo*' myimage mypackage

# Multiple rules
triton instance create --affinity 'instance!=db-*' --affinity 'role==webserver' myimage mypackage
```

### Future Enhancements (Not Implemented)

- Client-side validation of affinity rule syntax
- Short flag `-a` conflicts with global `--account` flag, so only `--long` form is available

## References

- [CloudAPI CreateMachine Documentation](https://apidocs.tritondatacenter.com/cloudapi/#CreateMachine)
- [Triton Docker Placement Documentation](https://apidocs.tritondatacenter.com/docker/features/placement)
- [node-triton do_create.js](target/node-triton/lib/do_instance/do_create.js)
- [sdc-cloudapi docs/index.md](target/sdc-cloudapi/docs/index.md) - Lines 5157-5227
