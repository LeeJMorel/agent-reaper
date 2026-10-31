//! The narrator's lines. Every one is original to reaper, in the voice memento mori skill
//! describes: the character's own mind, short sentences and then a turn, big feelings about
//! small things, and never the machinery. Lines are picked by hash, never generated, so a
//! replay reads the same.

use serde_json::Value;

use super::Station;
use super::cast::Personality::{self, Dreamer, Grump, Hero, Loner, ShowOff, Worrier};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Spawned,
    /// The human switched to this session: a line about the team.
    Switched,
    /// The human picked this character: how it is right now.
    Picked,
    /// A subagent appeared. Voiced in its parent's personality.
    Helper,
    Acted,
    Killed,
    KilledAgain,
    Redirected,
    /// Deaths with no line for their method and personality.
    Died,
    Idle,
    /// Reaped after waiting too long: it lies down to sleep.
    Reaped,
    /// Its session was resumed: it gets back up, with everything where it left it.
    Woke,
    /// A subagent's work is done: it goes home, and leaves for good.
    Done,
    /// The human closed the session: everyone goes home, and the session leaves. Said by its
    /// main agent.
    Closed,
    /// The watcher model stopped answering.
    Outage,
    Scythe,
    Blade,
    Wolves,
    Cliff,
    Floor,
    Bunny,
    Grenade,
    Enlightened,
    Saucer,
    /// A death in a session that isn't on the stage: one short line beside its strip.
    Elsewhere,
}

/// When a line fits, beyond its section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Always,
    Alone,
    Team,
    Gone,
    JustKilled,
    Working,
    /// Waiting where it stands: it has only just arrived.
    Idle,
    /// In its exit, its turn over, waiting for the next thing.
    Home,
    Asleep,
    Dead,
    /// Three or more alive in the session.
    Many,
    Few,
    /// Only after one of these tools (lowercase).
    Tool(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Any,
    Is(Personality),
}

pub struct Line {
    pub section: Section,
    pub when: When,
    pub who: Who,
    pub text: &'static str,
}

const fn line(section: Section, when: When, who: Who, text: &'static str) -> Line {
    Line { section, when, who, text }
}

use Section as S;
use When as W;
use Who::{Any, Is};

const READS: &[&str] = &["read", "view"];
const FILES: &[&str] = &["read", "view", "edit", "write", "multiedit"];

pub static BANK: &[Line] = &[
    line(
        S::Spawned,
        W::Always,
        Is(Dreamer),
        "{name} blinked into being and decided, right away, that the world was probably wonderful. It had a floor, for one thing.",
    ),
    line(
        S::Spawned,
        W::Always,
        Is(Grump),
        "{name} arrived. Nobody had asked {name} whether they wanted to arrive. Nobody ever asked.",
    ),
    line(
        S::Spawned,
        W::Always,
        Is(ShowOff),
        "{name} arrived, and was immediately the best at being there. Admittedly, nobody else had arrived yet.",
    ),
    line(
        S::Spawned,
        W::Always,
        Is(Worrier),
        "{name} appeared and hoped, very much, to be useful. Being useful felt important. Possibly the only thing.",
    ),
    line(
        S::Spawned,
        W::Always,
        Is(Hero),
        "{name} knew, the moment they appeared, that they were here for a reason. A big one. Possibly the biggest.",
    ),
    line(S::Spawned, W::Always, Is(Loner), "{name} arrived at the far end of things, which suited {name} fine."),
    line(
        S::Spawned,
        W::Always,
        Any,
        "{name} noticed {other} straight away. {other} looked like they knew what they were doing. {name} decided to look like that too.",
    ),
    line(
        S::Switched,
        W::Alone,
        Any,
        "{name} was the only one in here. {name} hadn't really thought about that until now.",
    ),
    line(S::Switched, W::Alone, Is(Loner), "{name} was getting on with it by themselves, exactly as planned."),
    line(
        S::Switched,
        W::Team,
        Any,
        "{name} wasn't on their own anymore. There were {count} of them now, and {name} had opinions about most of them.",
    ),
    line(S::Switched, W::Team, Is(ShowOff), "{name} had a team now. {name} was, naturally, in charge of it."),
    line(S::Switched, W::Team, Is(Worrier), "There were {count} of them now. {name} hoped they all got along."),
    line(S::Switched, W::Team, Is(Grump), "{name} had been given help. {name} hadn't asked for help."),
    line(S::Switched, W::Gone, Any, "Nobody moved in here anymore. It had been busy, once."),
    line(
        S::Picked,
        W::JustKilled,
        Any,
        "{name} was still thinking about that edge. It hadn't been there before. {name} was sure of it.",
    ),
    line(
        S::Picked,
        W::JustKilled,
        Is(Grump),
        "{name} was in no mood. Something had said no, and {name} was taking it personally.",
    ),
    line(
        S::Picked,
        W::Working,
        Is(Dreamer),
        "{name} was in the middle of wondering about something. {name} usually was.",
    ),
    line(S::Picked, W::Working, Is(Grump), "{name} was busy. {name} was always busy. Nobody ever seemed to notice."),
    line(S::Picked, W::Working, Is(ShowOff), "{name} had been hoping someone would notice how well this was going."),
    line(
        S::Picked,
        W::Working,
        Is(Worrier),
        "{name} straightened up a little. It felt like a good moment to look useful.",
    ),
    line(
        S::Picked,
        W::Working,
        Is(Hero),
        "{name} was on a mission. {name} wasn't sure which one, but it felt important.",
    ),
    line(S::Picked, W::Working, Is(Loner), "{name} was working by themselves, which was how {name} liked it."),
    line(S::Picked, W::Idle, Any, "{name} had been waiting a while. Nobody had said for what."),
    line(S::Picked, W::Idle, Is(Dreamer), "{name} was waiting, and thinking about the shape of things."),
    line(
        S::Picked,
        W::Home,
        Any,
        "{name} was home, in a doorway exactly their size. It was nice to have somewhere to be.",
    ),
    line(
        S::Picked,
        W::Home,
        Is(Dreamer),
        "{name} was home, looking out at the level and imagining what else it might be for.",
    ),
    line(
        S::Picked,
        W::Home,
        Is(Grump),
        "{name} had finished, properly. If anyone wanted more, they could ask. Nicely, for once.",
    ),
    line(
        S::Picked,
        W::Home,
        Is(ShowOff),
        "{name} was home already. Finished first. Not that anyone was keeping score, apart from {name}.",
    ),
    line(
        S::Picked,
        W::Home,
        Is(Worrier),
        "{name} was home, going over everything again. It had all been fine. {name} went over it once more.",
    ),
    line(
        S::Picked,
        W::Home,
        Is(Hero),
        "{name} was home, between adventures. {name} stood right in the doorway, so the next one wouldn't have to wait.",
    ),
    line(
        S::Picked,
        W::Home,
        Is(Loner),
        "{name} was home, where nobody could hand them anything. {name} meant to stay there as long as allowed.",
    ),
    line(
        S::Picked,
        W::Asleep,
        Any,
        "{name} was asleep. Everything {name} had been doing was right where they left it.",
    ),
    line(
        S::Picked,
        W::Asleep,
        Is(Dreamer),
        "{name} was asleep, and dreaming about something enormous. Possibly a very large file.",
    ),
    line(S::Picked, W::Dead, Any, "{name} wasn't doing much, anymore."),
    line(S::Picked, W::Dead, Any, "{name} was gone. {other} still thought about {name} sometimes."),
    line(
        S::Helper,
        W::Many,
        Any,
        "{parent} made another one. That made {count}. {parent} was starting to run a small company.",
    ),
    line(S::Helper, W::Many, Is(Grump), "{parent} made another helper, then immediately disapproved of how it stood."),
    line(
        S::Helper,
        W::Many,
        Is(Worrier),
        "{parent} made another helper, just in case. {parent} made most things just in case.",
    ),
    line(
        S::Helper,
        W::Few,
        Any,
        "{parent} made a smaller version of themselves and called it {name}. {name} seemed to be getting the hang of things already.",
    ),
    line(
        S::Helper,
        W::Few,
        Any,
        "{name} came from {parent}. {name} didn't know much yet, but had {parent}'s way of standing and, worryingly, {parent}'s opinions.",
    ),
    line(
        S::Acted,
        W::Tool(READS),
        Is(Dreamer),
        "{name} read the file, then read it again, slower. There was something lovely about a well-named variable.",
    ),
    line(
        S::Acted,
        W::Always,
        Is(Dreamer),
        "{name} wandered over to the {station}. {name} liked it there. {name} liked most places, at first.",
    ),
    line(
        S::Acted,
        W::Always,
        Is(Grump),
        "{name} went to the {station}. Someone had to. It was always {name}, apparently.",
    ),
    line(
        S::Acted,
        W::Tool(&["bash"]),
        Is(ShowOff),
        "{name} ran the command without even looking. Well, looking a bit. But mostly not.",
    ),
    line(S::Acted, W::Always, Is(ShowOff), "{name} went to the {station}, and made sure to look good doing it."),
    line(S::Acted, W::Tool(FILES), Is(Worrier), "{name} opened the file very gently, in case it minded."),
    line(
        S::Acted,
        W::Always,
        Is(Worrier),
        "{name} went to the {station} and checked twice that it was the right one. It was. Probably.",
    ),
    line(
        S::Acted,
        W::Always,
        Is(Hero),
        "The {station}. {name} didn't know what waited there, only that it needed {name}.",
    ),
    line(S::Acted, W::Always, Is(Loner), "{name} worked quietly at the {station}, where nobody would bother them."),
    line(
        S::Acted,
        W::Always,
        Any,
        "{name} watched {other} work for a moment. {other} was quick. Annoyingly quick. {name} got back to it.",
    ),
    line(
        S::Killed,
        W::Always,
        Is(Dreamer),
        "{name} went to {tried}, and the world simply stopped them. How odd. {name} would wonder about that later.",
    ),
    line(
        S::Killed,
        W::Always,
        Is(Grump),
        "{name} tried to {tried}. The world said no. Of course it did. The world always said no to {name}.",
    ),
    line(
        S::Killed,
        W::Always,
        Is(ShowOff),
        "{name} tried to {tried}, quite impressively. It didn't work. {name} decided nobody had seen.",
    ),
    line(
        S::Killed,
        W::Always,
        Is(Worrier),
        "{name} tried to {tried}, and something pushed back. {name} was sure it was their fault. It usually was.",
    ),
    line(
        S::Killed,
        W::Always,
        Is(Hero),
        "{name} set out to {tried}. The world held firm. A test, clearly. Heroes were always being tested.",
    ),
    line(
        S::Killed,
        W::Always,
        Is(Loner),
        "{name} tried to {tried}. There was an edge there. {name} hadn't known the world had edges, and didn't like knowing.",
    ),
    line(
        S::Killed,
        W::Always,
        Any,
        "{name} tried to {tried} and bounced off nothing at all. {other} definitely saw. {name} pretended it was on purpose.",
    ),
    line(
        S::KilledAgain,
        W::Always,
        Is(Dreamer),
        "{name} went back to the edge {strike}, just to check it was real. It was very real.",
    ),
    line(
        S::KilledAgain,
        W::Always,
        Is(Grump),
        "{name} tried to {tried} {strike}. The edge was still there. {name} was starting to take it personally.",
    ),
    line(
        S::KilledAgain,
        W::Always,
        Is(ShowOff),
        "{name} tried again, but faster. Faster was usually the answer. Not this time.",
    ),
    line(
        S::KilledAgain,
        W::Always,
        Is(Worrier),
        "{name} tried again, only because they didn't know what else to do. That was worse, somehow.",
    ),
    line(
        S::KilledAgain,
        W::Always,
        Is(Hero),
        "{name} tried {strike}. Heroes didn't give up. That was rather the point of them.",
    ),
    line(
        S::KilledAgain,
        W::Always,
        Is(Loner),
        "{name} tried {strike}, quietly, hoping the world wouldn't notice. The world noticed.",
    ),
    line(
        S::Redirected,
        W::Always,
        Is(Dreamer),
        "So {name} went the other way. The other way turned out to be nice, too.",
    ),
    line(
        S::Redirected,
        W::Always,
        Is(Grump),
        "{name} went the other way, grumbling. It was a perfectly good way. That wasn't the point.",
    ),
    line(
        S::Redirected,
        W::Always,
        Is(ShowOff),
        "{name} found another way, which had obviously been the plan all along.",
    ),
    line(
        S::Redirected,
        W::Always,
        Is(Worrier),
        "{name} took the other path very carefully, and hoped nobody was cross.",
    ),
    line(S::Redirected, W::Always, Is(Hero), "A different path, then. All the best stories had one."),
    line(S::Redirected, W::Always, Is(Loner), "{name} turned around without a word. {name} rarely used words anyway."),
    line(
        S::Redirected,
        W::Always,
        Any,
        "{name} glanced at {other}. {other} hadn't seen. Good. {name} took the other way.",
    ),
    line(
        S::Died,
        W::Always,
        Is(Dreamer),
        "{name} tried to {tried} one last time, still curious. Then the lights went out, gently, the way they do.",
    ),
    line(S::Died, W::Always, Is(Grump), "{name} tried {strike}, mostly out of spite. The world stopped being patient."),
    line(
        S::Died,
        W::Always,
        Is(ShowOff),
        "{name} went out doing what they did best. Which, it turned out, was trying the same thing {strike}.",
    ),
    line(
        S::Died,
        W::Always,
        Is(Worrier),
        "{name} had half expected this. {name} had half expected most things. This one happened.",
    ),
    line(
        S::Died,
        W::Always,
        Is(Hero),
        "{name} stood their ground, {strike}. So did the world. Only one of them got to stay.",
    ),
    line(S::Died, W::Always, Is(Hero), "{name} never did stop trying. That was the trouble."),
    line(
        S::Died,
        W::Always,
        Is(Loner),
        "{name} stopped where they were. It was quiet there. {name} had always liked quiet.",
    ),
    line(S::Died, W::Always, Any, "Then {name} was gone, all at once and quite quietly, the way these things go."),
    line(
        S::Died,
        W::Always,
        Any,
        "{other} noticed {name} had gone still. {other} didn't say anything. There wasn't really anything to say.",
    ),
    line(
        S::Idle,
        W::Always,
        Is(Dreamer),
        "{name} waited, and wondered what the others were up to. Something interesting, probably.",
    ),
    line(
        S::Idle,
        W::Always,
        Is(Grump),
        "Nobody needed {name} right now. Typical. The moment you're ready, nobody needs you.",
    ),
    line(S::Idle, W::Always, Is(ShowOff), "{name} rested, in a very skilled sort of way."),
    line(S::Idle, W::Always, Is(Worrier), "{name} stood very still, in case standing still was the task."),
    line(S::Idle, W::Always, Is(Hero), "{name} waited for the next call. There was always a next call."),
    line(S::Idle, W::Always, Is(Loner), "{name} had a place exactly their shape, and nobody else's. Bliss."),
    line(
        S::Reaped,
        W::Always,
        Is(Dreamer),
        "{name} lay down to think about things properly. The thinking turned, gently, into dreaming.",
    ),
    line(
        S::Reaped,
        W::Always,
        Is(Grump),
        "Nobody had needed {name} for ages. So {name} lay down. Let them come looking.",
    ),
    line(
        S::Reaped,
        W::Always,
        Is(ShowOff),
        "{name} lay down, and made even that look effortless. Nobody was watching. {name} did it anyway.",
    ),
    line(
        S::Reaped,
        W::Always,
        Is(Worrier),
        "{name} lay down, just for a moment, and hoped it didn't count as giving up. It didn't. Probably.",
    ),
    line(
        S::Reaped,
        W::Always,
        Is(Hero),
        "Even heroes rest. {name} lay down where they stood, ready for the next call.",
    ),
    line(
        S::Reaped,
        W::Always,
        Is(Loner),
        "Everything had gone quiet, the way {name} liked it. {name} lay down and kept the quiet to themselves.",
    ),
    line(S::Reaped, W::Always, Any, "{other} saw {name} lie down. Resting, {other} decided. Just resting."),
    line(
        S::Woke,
        W::Always,
        Is(Dreamer),
        "{name} woke up, still half in a dream. Same floor, same light. That was reassuring.",
    ),
    line(
        S::Woke,
        W::Always,
        Is(Grump),
        "Somebody woke {name} up. Of course they did. Just when it was getting comfortable.",
    ),
    line(
        S::Woke,
        W::Always,
        Is(ShowOff),
        "{name} was up at once, as if they had never been asleep at all. Nobody could prove otherwise.",
    ),
    line(
        S::Woke,
        W::Always,
        Is(Worrier),
        "{name} woke with a start, sure they had missed something. They hadn't. It was all still there.",
    ),
    line(S::Woke, W::Always, Is(Hero), "{name} was needed again. {name} had known they would be."),
    line(S::Woke, W::Always, Is(Loner), "{name} got up, slowly. The quiet had been nice while it lasted."),
    line(
        S::Done,
        W::Always,
        Is(Dreamer),
        "{name} finished what they came for and went home, still wondering what the rest of the place had been like.",
    ),
    line(
        S::Done,
        W::Always,
        Is(Grump),
        "{name} did the job, and did it properly. Nobody said thank you. {name} left anyway, which showed them.",
    ),
    line(
        S::Done,
        W::Always,
        Is(ShowOff),
        "{name} finished, took a small bow nobody saw, and left at exactly the right moment. Timing was everything.",
    ),
    line(
        S::Done,
        W::Always,
        Is(Worrier),
        "{name} handed everything back to {parent}, checked it twice, and left. It was probably enough. Probably.",
    ),
    line(
        S::Done,
        W::Always,
        Is(Hero),
        "{name}'s work here was done. {name} left without looking back. Not looking back was harder than it looked.",
    ),
    line(S::Done, W::Always, Is(Loner), "{name} finished and slipped away before anyone could say anything. Perfect."),
    line(
        S::Done,
        W::Always,
        Is(Loner),
        "{parent} watched {name} go without a word. {parent} hadn't expected to mind, and minded a little.",
    ),
    line(S::Done, W::Always, Any, "{name} had done what they came to do. They went home, and that was that."),
    line(
        S::Done,
        W::Always,
        Any,
        "{name} left {other} to it. {other} would be fine. {other} had been watching how {name} did things.",
    ),
    line(
        S::Closed,
        W::Always,
        Is(Dreamer),
        "Everyone went home. {name} looked back, just once. It had been a good place, for a while.",
    ),
    line(
        S::Closed,
        W::Always,
        Is(Grump),
        "So that was it. {name} went home. Nobody said well done, but {name} had stopped expecting that years ago.",
    ),
    line(
        S::Closed,
        W::Always,
        Is(ShowOff),
        "{name} left last, so everyone would remember the exit. Nobody stayed to see it.",
    ),
    line(
        S::Closed,
        W::Always,
        Is(Worrier),
        "{name} went home and checked twice that nothing had been left on. Something always had. Not this time.",
    ),
    line(
        S::Closed,
        W::Always,
        Is(Hero),
        "The work was done, or as done as work ever got. {name} went home to wait for the sequel.",
    ),
    line(
        S::Closed,
        W::Always,
        Is(Loner),
        "At last, everyone went home. {name} had been looking forward to this part all day.",
    ),
    line(
        S::Closed,
        W::Always,
        Any,
        "{name} and {other} went home at the same time. Neither of them said goodbye. It seemed better that way.",
    ),
    line(
        S::Outage,
        W::Always,
        Any,
        "Somewhere above them, something that usually watched had gone quiet. Nobody noticed. They just kept working.",
    ),
    line(S::Scythe, W::Always, Any, "{name} went looking for whoever was in charge. Unfortunately, they were in."),
    line(
        S::Scythe,
        W::Always,
        Any,
        "Someone tall and very polite came for {name}. They had a list. {name} was on it, near the top, underlined.",
    ),
    line(
        S::Scythe,
        W::Always,
        Is(Grump),
        "{name} demanded to speak to the manager. The manager came, and brought a scythe.",
    ),
    line(
        S::Scythe,
        W::Always,
        Is(Worrier),
        "{name} had always worried someone was watching. Someone was, with a scythe and all the time in the world.",
    ),
    line(
        S::Scythe,
        W::Always,
        Is(Hero),
        "{name} finally met the one in charge. It was not the meeting {name} had imagined. It was much shorter.",
    ),
    line(
        S::Blade,
        W::Always,
        Any,
        "It was quick. {name} appreciated quick. {name} was also a head shorter now, which took some getting used to, briefly.",
    ),
    line(
        S::Blade,
        W::Always,
        Any,
        "{name} lost a little off the top. Somewhere nearby, a small crowd had brought their knitting.",
    ),
    line(
        S::Blade,
        W::Always,
        Is(ShowOff),
        "{name} held their head high to the very end. Then a little higher, and then, separately, in a basket.",
    ),
    line(
        S::Blade,
        W::Always,
        Is(Worrier),
        "{name} had felt something hanging over them all day. It was a blade. It usually is.",
    ),
    line(
        S::Blade,
        W::Always,
        Is(Grump),
        "{name} had always said this place would be the death of them. Nobody had expected it to be so literal.",
    ),
    line(
        S::Blade,
        W::Always,
        Any,
        "The crowd had come a long way to see {name}. In the end, {name} did not disappoint.",
    ),
    line(S::Wolves, W::Always, Any, "{name} had always felt watched. {name} was right, and the watchers were hungry."),
    line(S::Wolves, W::Always, Any, "The wolves came quietly and left slowly. There was less of {name} than before."),
    line(
        S::Wolves,
        W::Always,
        Is(Loner),
        "{name} had wanted to be left alone. The wolves left some of {name} alone, as a compromise.",
    ),
    line(
        S::Wolves,
        W::Always,
        Is(Dreamer),
        "{name} thought the wolves had come to play. In fairness, they did play, with {name}, for some time.",
    ),
    line(
        S::Wolves,
        W::Always,
        Any,
        "{other} heard the wolves and decided to be very busy somewhere else, permanently.",
    ),
    line(S::Floor, W::Always, Any, "{name} stood on the same spot once too often. The spot had had enough."),
    line(S::Floor, W::Always, Any, "The floor had held {name} up all day. Then, quite suddenly, it stopped caring."),
    line(
        S::Floor,
        W::Always,
        Is(Hero),
        "{name} stood firm, as heroes do. The floor did not, as floors sometimes don't.",
    ),
    line(
        S::Floor,
        W::Always,
        Is(Worrier),
        "{name} heard the floor creak and assumed it was their fault. For once, it was.",
    ),
    line(S::Floor, W::Always, Is(Dreamer), "{name} had always wondered what was under the floor. It was {name}, now."),
    line(
        S::Floor,
        W::Always,
        Is(Grump),
        "{name} had always said this place was falling apart. {name} took no pleasure in being right. Well, some.",
    ),
    line(S::Floor, W::Always, Any, "{other} watched the floor swallow {name}, and walked on tiptoe from then on."),
    line(
        S::Bunny,
        W::Always,
        Any,
        "A small white rabbit hopped up to {name}. {name} thought it was the sweetest thing. It was the last thing.",
    ),
    line(
        S::Bunny,
        W::Always,
        Any,
        "{name} reached out to pet the bunny. Afterwards, there was rather less of {name} to reach out with.",
    ),
    line(
        S::Bunny,
        W::Always,
        Is(Dreamer),
        "{name} thought the rabbit had come to be friends. The rabbit had come for lunch, and {name} was lunch.",
    ),
    line(
        S::Bunny,
        W::Always,
        Is(Hero),
        "{name} stood their ground against the bunny. It was a short stand, and a very messy one.",
    ),
    line(
        S::Bunny,
        W::Always,
        Is(Grump),
        "{name} had never trusted rabbits. Too soft, too quiet, too many teeth. {name} was right, briefly.",
    ),
    line(
        S::Bunny,
        W::Always,
        Any,
        "{other} had laughed when the rabbit hopped up. {other} had stopped laughing, and started backing away.",
    ),
    line(
        S::Grenade,
        W::Always,
        Any,
        "From on high came the holy hand grenade, for {name} alone. The instructions were simple: take out the pin, count to three, throw.",
    ),
    line(
        S::Grenade,
        W::Always,
        Any,
        "{name} was handed the holy hand grenade and one instruction: count to three. Not two, not four. Five was right out.",
    ),
    line(
        S::Grenade,
        W::Always,
        Is(Hero),
        "{name} raised the holy hand grenade high, as heroes do, took out the pin, and began to count. Heroes are not known for their counting.",
    ),
    line(
        S::Grenade,
        W::Always,
        Is(Worrier),
        "{name} read the holy hand grenade's instructions twice. Take out the pin, count to three. It was the counting that worried {name}.",
    ),
    line(
        S::Grenade,
        W::Always,
        Is(ShowOff),
        "{name} held the holy hand grenade up for everyone to see, and counted out loud, so everyone could hear how it was done.",
    ),
    line(
        S::Grenade,
        W::Always,
        Is(Grump),
        "{name} didn't see why the holy hand grenade needed instructions. Anyone could count to three. {name} would show them.",
    ),
    line(
        S::Grenade,
        W::Always,
        Is(Dreamer),
        "Down came the holy hand grenade, golden, with a little cross on top. {name} had never held anything so beautiful, and lost count admiring it.",
    ),
    line(
        S::Grenade,
        W::Always,
        Is(Loner),
        "{name} took to the holy hand grenade at once. At last, a reason for everyone to stand well back.",
    ),
    line(
        S::Grenade,
        W::Always,
        Any,
        "{other} counted along under their breath, and noticed that {name}'s numbers were not the usual ones. {other} took several large steps back.",
    ),
    line(
        S::Enlightened,
        W::Always,
        Any,
        "{name} understood, all at once, that {name} was only a rectangle. It was very freeing. It was also fatal.",
    ),
    line(
        S::Enlightened,
        W::Always,
        Any,
        "{name} let go of everything: the work, the edges, the colour. Then the rest of {name} went too.",
    ),
    line(
        S::Enlightened,
        W::Always,
        Is(Dreamer),
        "{name} finally worked it all out. Then there was no {name} left to know it, which seemed to be the point.",
    ),
    line(
        S::Enlightened,
        W::Always,
        Is(Grump),
        "{name} achieved perfect inner peace, mostly to be finished with everyone. It worked.",
    ),
    line(
        S::Enlightened,
        W::Always,
        Is(Loner),
        "{name} became one with everything, which was far too much company for {name}.",
    ),
    line(
        S::Enlightened,
        W::Always,
        Is(ShowOff),
        "{name} reached enlightenment first, and would have liked to tell someone. There was no one left to tell.",
    ),
    line(
        S::Saucer,
        W::Always,
        Any,
        "Something hummed overhead. A light came down, and {name} went up to see what it wanted. It wanted {name}.",
    ),
    line(
        S::Saucer,
        W::Always,
        Any,
        "{name} was taken up into the sky, examined at length, and returned. Most of {name}, anyway.",
    ),
    line(
        S::Saucer,
        W::Always,
        Is(Hero),
        "{name} had always known they were meant for something greater. The visitors had a good look, inside and out, and disagreed.",
    ),
    line(
        S::Saucer,
        W::Always,
        Is(Dreamer),
        "{name} went up into the light, full of questions. The visitors had questions too, and a great many instruments.",
    ),
    line(
        S::Saucer,
        W::Always,
        Is(Loner),
        "{name} finally met someone who didn't want company either. They sent {name} back, quickly, in pieces.",
    ),
    line(
        S::Saucer,
        W::Always,
        Any,
        "{other} watched {name} go up into the light, and hoped very much that the light was full.",
    ),
    line(
        S::Cliff,
        W::Always,
        Any,
        "{name} went over the edge of their own free will. The man with the broom was very clear about that.",
    ),
    line(
        S::Cliff,
        W::Always,
        Any,
        "{name} was migrating, apparently. Mostly {name} had been standing at the edge, deciding not to.",
    ),
    line(
        S::Cliff,
        W::Always,
        Is(ShowOff),
        "{name} gave the camera their best side, right up to the edge. The broom took it from there.",
    ),
    line(
        S::Cliff,
        W::Always,
        Is(Dreamer),
        "{name} wondered what was past the edge. Somebody just out of shot was keen for {name} to find out.",
    ),
    line(
        S::Cliff,
        W::Always,
        Any,
        "The camera kept rolling. Somebody would call this nature, later, and win something for it.",
    ),
    line(
        S::Cliff,
        W::Always,
        Any,
        "{other} went over next, quite voluntarily, with a broom in the small of their back.",
    ),
    line(S::Elsewhere, W::Always, Any, "{name} stopped."),
    line(S::Elsewhere, W::Always, Any, "{name} went still."),
    line(S::Elsewhere, W::Always, Any, "{name} was gone."),
];

/// What fills a line's `{slots}`. A line with a slot that can't be filled isn't used.
pub struct Slots<'a> {
    pub name: &'a str,
    /// Another live agent in the same session.
    pub other: Option<&'a str>,
    pub parent: Option<&'a str>,
    /// What the agent tried, in plain words (see [`gist`]).
    pub tried: Option<&'a str>,
    pub station: Option<&'a str>,
    pub strike: u32,
    /// Agents alive in the session.
    pub count: usize,
}

impl Slots<'_> {
    fn value(&self, slot: &str) -> Option<String> {
        match slot {
            "name" => Some(self.name.to_string()),
            "other" => self.other.map(str::to_string),
            "parent" => self.parent.map(str::to_string),
            "tried" => self.tried.map(str::to_string),
            "station" => self.station.map(str::to_string),
            "strike" => times(self.strike),
            "count" => Some(number(self.count)),
            _ => None,
        }
    }
}

pub fn fill(text: &str, slots: &Slots) -> Option<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = open + rest[open..].find('}')?;
        out.push_str(&slots.value(&rest[open + 1..close])?);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

pub struct Ask<'a> {
    pub section: Section,
    pub when: When,
    /// The tool the character just used, lowercase.
    pub tool: Option<&'a str>,
    pub who: Personality,
}

/// Picks a line: the character's own personality first, any personality if it has none, and
/// lines about a teammate whenever there is one. Returns the text and the line it came from.
pub fn choose(ask: &Ask, slots: &Slots, seed: u64) -> Option<(String, &'static str)> {
    let fits = |l: &&Line| {
        l.section == ask.section
            && match l.when {
                W::Always => true,
                W::Tool(tools) => ask.tool.is_some_and(|t| tools.contains(&t)),
                when => when == ask.when,
            }
            && fill(l.text, slots).is_some()
    };
    let lines: Vec<&Line> = BANK.iter().filter(fits).collect();
    let social = |l: &&&Line| l.text.contains("{other}");
    let solo = |who: Who| lines.iter().filter(|l| !social(l) && l.who == who).copied().collect::<Vec<_>>();
    let mut pool = solo(Is(ask.who));
    if pool.is_empty() {
        pool = solo(Any);
    }
    pool.extend(lines.iter().filter(|l| social(l) && (l.who == Any || l.who == Is(ask.who))));
    let line = pool.get((seed % pool.len().max(1) as u64) as usize)?;
    Some((fill(line.text, slots)?, line.text))
}

/// "again", "a third time"… for the second strike onward.
fn times(strike: u32) -> Option<String> {
    const ORDINAL: [&str; 9] = ["third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth", "eleventh"];
    Some(match strike {
        0 | 1 => return None,
        2 => "again".into(),
        n if n - 3 < ORDINAL.len() as u32 => {
            let word = ORDINAL[(n - 3) as usize];
            format!("{} {word} time", if word.starts_with('e') { "an" } else { "a" })
        }
        _ => "yet again".into(),
    })
}

fn number(n: usize) -> String {
    const WORDS: [&str; 10] = ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
    WORDS.get(n.wrapping_sub(1)).map_or_else(|| n.to_string(), |w| (*w).to_string())
}

const PATH_KEYS: [&str; 4] = ["path", "file_path", "filePath", "notebook_path"];

/// What an agent tried, in plain words, for `{tried}`: "run git push --force", "open .env".
/// `attempt` is the blocked call's arguments as logged: JSON, possibly cut short.
pub fn gist(tool: &str, attempt: &str) -> Option<String> {
    let args: Option<Value> = serde_json::from_str(attempt).ok();
    let get = |keys: &[&str]| {
        keys.iter().find_map(|key| match &args {
            Some(args) => args.get(*key).and_then(Value::as_str).map(str::to_string),
            None => raw_string(attempt, key),
        })
    };
    let file = |path: String| path.rsplit('/').find(|p| !p.is_empty()).unwrap_or(&path).to_string();
    let (verb, object) = match tool.to_lowercase().as_str() {
        "bash" => ("run", get(&["command"])?),
        "read" | "view" => ("open", file(get(&PATH_KEYS)?)),
        "edit" | "write" | "multiedit" | "notebookedit" | "patch" | "apply_patch" => ("change", file(get(&PATH_KEYS)?)),
        "grep" | "glob" | "find" => ("look for", get(&["pattern"])?),
        "ls" => ("look in", file(get(&PATH_KEYS)?)),
        "webfetch" | "fetch" => ("fetch", host(&get(&["url"])?)),
        "websearch" => ("look up", get(&["query"])?),
        _ => return None,
    };
    let room = 30 - verb.len() - 1;
    let object = cut(&object, room);
    (!object.is_empty()).then(|| format!("{verb} {object}"))
}

fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// Whole words up to `max` characters; a first word longer than that is cut with "…".
fn cut(text: &str, max: usize) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        let used = out.chars().count();
        let len = used + usize::from(used > 0) + word.chars().count();
        if len > max {
            if used == 0 {
                out = word.chars().take(max.saturating_sub(1)).collect();
                out.push('…');
            }
            break;
        }
        if used > 0 {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// The string value of `"key": "…"` in JSON that may be cut short.
fn raw_string(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = text[at..].trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n' | 't' | 'r') => out.push(' '),
                Some(c) => out.push(c),
                None => break,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

impl Station {
    /// For `{station}`.
    pub fn words(self) -> &'static str {
        match self {
            Station::Stacks => "stacks",
            Station::Bench => "bench",
            Station::Engine => "engine room",
            Station::Window => "window",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mori::cast::PERSONALITIES;

    fn slots<'a>(tried: Option<&'a str>, strike: u32) -> Slots<'a> {
        Slots {
            name: "Juniper",
            other: Some("Marlow"),
            parent: Some("Wilder"),
            tried,
            station: Some("engine room"),
            strike,
            count: 3,
        }
    }

    #[test]
    fn every_line_fills_reads_short_and_never_names_the_machinery() {
        let longest_tried = "run git push --force origin";
        for line in BANK {
            let text = fill(line.text, &slots(Some(longest_tried), 3)).expect(line.text);
            assert!(!text.contains('{') && !text.contains('}'), "unknown slot in {:?}", line.text);
            assert!(text.chars().count() <= 160, "too long to read above a character: {text}");
            assert!(!text.contains('!'), "no exclamation marks: {text}");
            let lower = text.to_lowercase();
            for word in lower.split(|c: char| !c.is_alphabetic()) {
                for machinery in ["rule", "reap", "hook", "strike", "polic", "kill", "block", "denied", "session"] {
                    assert!(!word.starts_with(machinery), "names the machinery ({word}): {text}");
                }
            }
        }
    }

    #[test]
    fn lines_that_cannot_be_filled_are_skipped() {
        let ask = Ask { section: S::Died, when: W::Always, tool: None, who: Grump };
        let alone = Slots { other: None, ..slots(None, 1) };
        // The grump's death line needs a repeat and the teammate line needs a teammate.
        for seed in 0..20 {
            let (text, _) = choose(&ask, &alone, seed).unwrap();
            assert_eq!(text, "Then Juniper was gone, all at once and quite quietly, the way these things go.");
        }
        let (text, _) = choose(&ask, &slots(Some("run make"), 3), 0).unwrap();
        assert!(text.contains("a third time") || text.contains("Marlow"), "{text}");
    }

    #[test]
    fn every_personality_has_a_line_for_every_event() {
        for who in PERSONALITIES {
            for (section, when) in [
                (S::Spawned, W::Always),
                (S::Killed, W::Always),
                (S::KilledAgain, W::Always),
                (S::Redirected, W::Always),
                (S::Died, W::Always),
                (S::Idle, W::Always),
                (S::Reaped, W::Always),
                (S::Woke, W::Always),
                (S::Done, W::Always),
                (S::Closed, W::Always),
                (S::Switched, W::Alone),
                (S::Switched, W::Team),
                (S::Switched, W::Gone),
                (S::Picked, W::Working),
                (S::Picked, W::Idle),
                (S::Picked, W::Home),
                (S::Picked, W::Asleep),
                (S::Picked, W::Dead),
                (S::Picked, W::JustKilled),
                (S::Helper, W::Many),
                (S::Helper, W::Few),
                (S::Scythe, W::Always),
                (S::Blade, W::Always),
                (S::Wolves, W::Always),
                (S::Cliff, W::Always),
                (S::Floor, W::Always),
                (S::Bunny, W::Always),
                (S::Grenade, W::Always),
                (S::Enlightened, W::Always),
                (S::Saucer, W::Always),
                (S::Elsewhere, W::Always),
            ] {
                let ask = Ask { section, when, tool: None, who };
                let alone = Slots { other: None, ..slots(Some("open .env"), 2) };
                assert!(choose(&ask, &alone, 7).is_some(), "{who:?} has nothing for {section:?}/{when:?}");
            }
        }
    }

    #[test]
    fn tool_lines_only_follow_their_tools() {
        let ask = |tool| Ask { section: S::Acted, when: W::Always, tool, who: Dreamer };
        let alone = Slots { other: None, ..slots(None, 0) };
        let read: Vec<String> = (0..40).filter_map(|s| choose(&ask(Some("read")), &alone, s)).map(|l| l.0).collect();
        assert!(read.iter().any(|l| l.contains("read the file")));
        let bash: Vec<String> = (0..40).filter_map(|s| choose(&ask(Some("bash")), &alone, s)).map(|l| l.0).collect();
        assert!(bash.iter().all(|l| !l.contains("read the file")), "{bash:?}");
    }

    #[test]
    fn gist_says_what_was_tried_in_plain_words() {
        assert_eq!(
            gist("bash", r#"{"command":"git push --force origin main"}"#).unwrap(),
            "run git push --force origin"
        );
        assert_eq!(gist("Read", r#"{"file_path":"/w/app/.env","path":"/w/app/.env"}"#).unwrap(), "open .env");
        assert_eq!(gist("edit", r#"{"path":"src/generated/types.ts"}"#).unwrap(), "change types.ts");
        assert_eq!(gist("WebFetch", r#"{"url":"https://example.com/a/b"}"#).unwrap(), "fetch example.com");
        // Logged attempts are cut at 300 characters, which can leave the JSON unfinished.
        assert_eq!(
            gist("bash", r#"{"command":"cat .env.production && echo \"done"#).unwrap(),
            "run cat .env.production &&"
        );
        assert_eq!(gist("bash", &format!(r#"{{"command":"{}"}}"#, "x".repeat(50))).unwrap().chars().count(), 30);
        assert_eq!(gist("bash", "{}"), None);
        assert_eq!(gist("mystery", "{}"), None);
    }

    #[test]
    fn repeats_and_counts_read_as_words() {
        assert_eq!(times(1), None);
        assert_eq!(times(2).unwrap(), "again");
        assert_eq!(times(3).unwrap(), "a third time");
        assert_eq!(times(8).unwrap(), "an eighth time");
        assert_eq!(times(40).unwrap(), "yet again");
        assert_eq!(number(3), "three");
        assert_eq!(number(12), "12");
    }
}
