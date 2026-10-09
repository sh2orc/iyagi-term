import { describe, expect, it } from "vitest";
import { dollarsToMicros, missionPolicy, missionRoleBindings, newBinding, parseCommandArgs, policyWithReviewMode, requiredMissionRoles } from "./configuration";

describe("USD cap conversion",()=>{
  it("keeps six decimal places without floating-point rounding",()=>{
    expect(dollarsToMicros("5.25")).toBe("5250000");
    expect(dollarsToMicros("0.000001")).toBe("1");
    expect(dollarsToMicros("9223372036854.775807")).toBe("9223372036854775807");
    expect(dollarsToMicros("   ")).toBeNull();
  });
  it("rejects malformed, non-positive, overly precise, or overflowing values",()=>{
    for(const input of ["NaN","Infinity","-1","0","1e3","0.0000001","9223372036854.775808"]){expect(()=>dollarsToMicros(input)).toThrow();}
  });
});

describe("verification argument parsing",()=>{
  it("treats blank input as no arguments",()=>{
    expect(parseCommandArgs("")).toEqual([]);
    expect(parseCommandArgs("   \t ")).toEqual([]);
  });
  it("splits plain text on any run of whitespace",()=>{
    expect(parseCommandArgs("test -- --run")).toEqual(["test","--","--run"]);
    expect(parseCommandArgs("  run   lint\t--fix \n")).toEqual(["run","lint","--fix"]);
  });
  it("groups single- and double-quoted text without interpreting escapes",()=>{
    expect(parseCommandArgs(`test "a b" 'c "d"'`)).toEqual(["test","a b",'c "d"']);
    expect(parseCommandArgs(`--name="two words" x`)).toEqual(["--name=two words","x"]);
    expect(parseCommandArgs(`"" ''`)).toEqual(["",""]);
    expect(parseCommandArgs(String.raw`C:\repo\tool.exe "C:\Program Files\x"`)).toEqual([String.raw`C:\repo\tool.exe`,String.raw`C:\Program Files\x`]);
  });
  it("rejects unbalanced quotes",()=>{
    for(const input of [`test "a b`,`'open`,`a "b' c`]){expect(()=>parseCommandArgs(input)).toThrow();}
  });
  it("accepts a JSON array of strings, including surrounding whitespace",()=>{
    expect(parseCommandArgs(`  ["test", "--", "--run"]  `)).toEqual(["test","--","--run"]);
    expect(parseCommandArgs("[]")).toEqual([]);
    expect(parseCommandArgs(`["a b"]`)).toEqual(["a b"]);
  });
  it("rejects JSON that is malformed or not an array of strings",()=>{
    for(const input of [`["test",`,`[1, 2]`,`["ok", null]`,`[["nested"]]`,`[ "a" "b"]`]){expect(()=>parseCommandArgs(input)).toThrow();}
  });
  it("splits bracketed text that is not a JSON array on whitespace",()=>{
    expect(parseCommandArgs("[slow] tests")).toEqual(["[slow]","tests"]);
    expect(parseCommandArgs(`[ci] "a b" --run`)).toEqual(["[ci]","a b","--run"]);
    expect(parseCommandArgs("[--grep=x]")).toEqual(["[--grep=x]"]);
    expect(()=>parseCommandArgs(`[slow] "open`)).toThrow();
  });
});

describe("review mode (계약 B)",()=>{
  it("requires lead·builder·reviewer with review and lead·builder without; integrator only when the team has one",()=>{
    // 데몬 start 검사와 같다 — integrator는 선택 역할이라 팀에 없으면 요구하지 않는다.
    expect(requiredMissionRoles(true,["lead","builder"])).toEqual(["lead","builder","reviewer"]);
    expect(requiredMissionRoles(true,["lead","builder","reviewer","integrator"])).toEqual(["lead","builder","reviewer","integrator"]);
    expect(requiredMissionRoles(false,["lead","builder","reviewer","integrator"])).toEqual(["lead","builder","integrator"]);
    expect(requiredMissionRoles(false,["lead","builder"])).toEqual(["lead","builder"]);
  });
  it("turns independent review off and drops reviewer from allowed roles and role bindings",()=>{
    const policy=missionPolicy(["b1"]);
    const skipped=policyWithReviewMode(policy,false);
    expect(skipped.require_independent_review).toBe(false);
    expect(skipped.allowed_roles).toEqual(["lead","builder","integrator"]);
    expect(skipped.allowed_binding_ids).toEqual(["b1"]);
    expect(policyWithReviewMode({...policy,require_independent_review:false},true)).toMatchObject({require_independent_review:true,allowed_roles:policy.allowed_roles});
    const bindings=(["lead","builder","reviewer","integrator"] as const).map(role=>({role,primary_binding_id:"b1",fallback_binding_ids:[]}));
    expect(missionRoleBindings(bindings,false).map(entry=>entry.role)).toEqual(["lead","builder","integrator"]);
    expect(missionRoleBindings(bindings,true)).toEqual(bindings);
  });
});

describe("newBinding",()=>{
  it("starts without experimental consent and without local evidence",()=>{
    // 로컬 증거는 데몬만 쓴다(11 §2): 새 연결은 빈 값으로 보내고 저장된 것을 이어 간다.
    expect(newBinding().experimental_version).toBeNull();
    expect(newBinding().local_evidence).toBeNull();
  });
});
