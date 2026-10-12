import { describeSkillTools, skillLabel } from "../lib/skills";
import { useConfig } from "../stores/config";

/**
 * 输入框里的技能选择器。
 *
 * 与旁边的角色选择器**刻意不同**：
 * - 角色是**会话级**的（挂在这个会话上，选一次影响整段对话）；
 * - 技能是**一次性**的（只作用于下一条消息，发完就清掉）。
 *
 * 所以这里选中的东西由输入框自己持有（`pendingSkillId`），不写进会话 ——
 * 写进去就会变成「这条会话一直被翻译技能笼罩」，而用户只想翻那一段话。
 *
 * 另一条用法是直接手打 `/名字`，效果一样（见 `lib/skills.ts`）。
 */
export function SkillPicker({
  value,
  onChange,
}: {
  value: string | null;
  onChange: (skillId: string | null) => void;
}) {
  const config = useConfig((s) => s.config);
  const skills = config?.skills ?? [];
  if (skills.length === 0) return null;

  const current = skills.find((s) => s.id === value) ?? null;

  return (
    <select
      className="skill-picker"
      value={value ?? ""}
      title={
        current
          ? `${current.description || "一次性任务指令"}｜${describeSkillTools(current)}`
          : "这次用什么技能（只影响下一条消息，也可以直接打 /名字）"
      }
      onChange={(e) => onChange(e.target.value === "" ? null : e.target.value)}
    >
      <option value="">不用技能</option>
      {skills.map((skill) => (
        <option key={skill.id} value={skill.id}>
          {skillLabel(skill)}
        </option>
      ))}
    </select>
  );
}
